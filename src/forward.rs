use std::time::Duration;

use log::{debug, warn};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    time::{sleep, timeout},
};

use crate::server::ServerInfo;

static TIME_OUT: tokio::time::Duration = tokio::time::Duration::from_secs(3);

pub struct Forwarder {
    pub server: ServerInfo,

    udp_socket: Option<UdpSocket>,
    tcp_socket: Option<TcpStream>,
}

impl Forwarder {
    pub fn new(server: ServerInfo) -> Self {
        Self {
            server,
            udp_socket: None,
            tcp_socket: None,
        }
    }

    async fn send_udp(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let mut retry_count = 0;

        loop {
            if retry_count >= 3 {
                return Err(format!("udp send data failed after {} retries.", retry_count).into());
            }

            self.connect_remote_server().await?;

            let udp_socket = self.udp_socket.as_mut().unwrap();
            match udp_socket.send(&data).await {
                Ok(size) => {
                    if size < data.len() {
                        self.udp_socket = None;
                        return Err("udp send data failed.".into());
                    }
                }
                Err(e) => {
                    self.udp_socket = None;
                    return Err(e.into());
                }
            }

            let mut buff = [0; 1024];
            match tokio::time::timeout(TIME_OUT, udp_socket.recv_from(&mut buff)).await {
                Ok(Ok((size, _))) => {
                    return Ok(buff[..size].to_vec());
                }
                Ok(Err(e)) => {
                    self.udp_socket = None;
                    return Err(e.into());
                }
                Err(_) => {
                    retry_count += 1;
                    continue;
                }
            }
        }
    }

    async fn send_tcp(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let mut retry_count = 0;

        loop {
            if retry_count >= 3 {
                return Err(format!("tcp send data failed after {} retries.", retry_count).into());
            }

            self.connect_remote_server().await?;

            let tcp_server = self.tcp_socket.as_mut().unwrap();

            let size = data.len() as u16;
            match tcp_server.write(&size.to_be_bytes()).await {
                Ok(r) => {
                    if r < size_of::<u16>() {
                        warn!("forward data failed. {}", size);
                        self.tcp_socket = None;
                        retry_count += 1;
                        continue;
                    }
                }
                Err(e) => {
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                    ) {
                        warn!("tcp write size failed. {}", e);
                    }

                    self.tcp_socket = None;
                    retry_count += 1;
                    continue;
                }
            }

            match tcp_server.write(data).await {
                Ok(r) => {
                    if r < data.len() {
                        warn!("forward data failed. {}", size);
                        self.tcp_socket = None;
                        retry_count += 1;
                        continue;
                    }
                }
                Err(e) => {
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                    ) {
                        warn!("tcp write size failed. {}", e);
                    }

                    self.tcp_socket = None;
                    retry_count += 1;
                    continue;
                }
            }

            let size = match tokio::time::timeout(TIME_OUT, tcp_server.read_u16()).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                    ) {
                        warn!("tcp read size failed. {}", e);
                    }

                    self.tcp_socket = None;
                    retry_count += 1;
                    continue;
                }
                Err(_) => {
                    self.tcp_socket = None;
                    retry_count += 1;
                    continue;
                }
            };

            let mut buff = vec![0 as u8; size as usize];
            match tokio::time::timeout(TIME_OUT, tcp_server.read_exact(&mut buff)).await {
                Ok(Ok(s)) => {
                    if usize::from(s) < size_of_val(&buff) {
                        warn!("tcp read data failed.");
                        self.tcp_socket = None;
                        retry_count += 1;
                        continue;
                    }
                }
                Ok(Err(e)) => {
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                    ) {
                        warn!("tcp read data failed. {}", e);
                    }

                    self.tcp_socket = None;
                    retry_count += 1;
                    continue;
                }
                Err(_) => {
                    self.tcp_socket = None;
                    retry_count += 1;
                    continue;
                }
            }

            return Ok(buff);
        }
    }

    pub async fn send(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        if self.server.is_tcp {
            self.send_tcp(data).await
        } else {
            self.send_udp(data).await
        }
    }

    async fn connect_remote_server(
        &mut self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut retry_count = 0;

        loop {
            if self.server.is_tcp {
                if self.tcp_socket.is_some() {
                    return Ok(());
                }

                debug!("connect tcp {}", self.server.addr);
                match timeout(
                    Duration::from_secs(3),
                    TcpStream::connect(&self.server.addr),
                )
                .await
                {
                    Ok(Ok(s)) => {
                        self.tcp_socket = Some(s);
                        return Ok(());
                    }
                    Ok(Err(_)) => {
                        warn!(
                            "connect {} failed. try again later. retry count: {}",
                            self.server.addr, retry_count
                        );
                    }
                    Err(_) => {
                        warn!(
                            "connect {} timeout. try again later. retry count: {}",
                            self.server.addr, retry_count
                        );
                    }
                }

                if retry_count >= 3 {
                    break;
                }

                retry_count += 1;
                sleep(Duration::from_secs(1)).await;
                continue;
            }

            if self.udp_socket.is_some() {
                return Ok(());
            }

            if let Ok(s) = UdpSocket::bind("0.0.0.0:0").await {
                if let Err(e) = s.connect(&self.server.addr).await {
                    return Err(format!("udp connect {} failed. {}", self.server.addr, e).into());
                }
                self.udp_socket = s.into();
                return Ok(());
            }

            return Err(format!("bind {} failed. try again later.", self.server.addr).into());
        }

        Err("connect remote server failed.".into())
    }
}
