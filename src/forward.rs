use crate::server::ServerInfo;
use anyhow::{bail, Result};
use bytes::{Bytes, BytesMut};
use log::{debug, warn};
use std::{io::IoSlice, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    time::timeout,
};

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

    async fn send_udp(&mut self, data: &[u8]) -> Result<Bytes> {
        self.connect_remote_server().await?;

        let udp_socket = self.udp_socket.as_mut().unwrap();
        match udp_socket.send(&data).await {
            Ok(size) => {
                if size < data.len() {
                    self.udp_socket = None;
                    bail!("udp send data failed.");
                }
            }
            Err(e) => {
                self.udp_socket = None;
                bail!("{e:?}");
            }
        }

        let mut buff = BytesMut::with_capacity(1024);
        buff.resize(1024, 0);

        match tokio::time::timeout(TIME_OUT, udp_socket.recv_from(&mut buff)).await {
            Ok(Ok((size, _))) => {
                buff.truncate(size);
                return Ok(buff.freeze());
            }
            Ok(Err(e)) => {
                self.udp_socket = None;
                bail!("{e:?}");
            }
            Err(_) => {
                bail!("udp recv data timeout.");
            }
        }
    }

    async fn send_tcp(&mut self, data: &[u8]) -> Result<Bytes> {
        self.connect_remote_server().await?;

        let tcp_server = self.tcp_socket.as_mut().unwrap();

        let size = (data.len() as u16).to_be_bytes();
        let bufs = &[IoSlice::new(&size), IoSlice::new(data)];

        match tcp_server.write_vectored(bufs).await {
            Ok(size) => {
                if size < size_of::<u16>() + data.len() {
                    self.tcp_socket = None;
                    bail!("forward data failed. {size}");
                }
            }
            Err(e) => {
                // if !matches!(
                //     e.kind(),
                //     std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                // ) {
                //     warn!("tcp write failed. {}", e);
                // }
                self.tcp_socket = None;
                bail!("tcp write failed. {e:?}");
            }
        }

        let size = match tokio::time::timeout(TIME_OUT, tcp_server.read_u16()).await {
            Ok(Ok(s)) => s as usize,
            Ok(Err(e)) => {
                // if !matches!(
                //     e.kind(),
                //     std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                // ) {
                //     warn!("tcp read size failed. {}", e);
                // }

                self.tcp_socket = None;
                bail!("tcp read size failed. {e:?}");
            }
            Err(_) => {
                self.tcp_socket = None;
                bail!("tcp read timeout");
            }
        };

        let mut buff = BytesMut::with_capacity(1024);
        buff.resize(size, 0);

        match tokio::time::timeout(TIME_OUT, tcp_server.read_exact(&mut buff)).await {
            Ok(Ok(s)) => {
                if usize::from(s) < size_of_val(&buff) {
                    self.tcp_socket = None;
                    bail!("tcp read data failed. size: {s}");
                }
            }
            Ok(Err(e)) => {
                // if !matches!(
                //     e.kind(),
                //     std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                // ) {
                //     warn!("tcp read data failed. {}", e);
                // }

                self.tcp_socket = None;
                bail!("tcp read data failed. {e:?}");
            }
            Err(_) => {
                self.tcp_socket = None;
                bail!("tcp read data timeout");
            }
        }

        return Ok(buff.freeze());
    }

    pub async fn send(&mut self, data: &[u8]) -> Result<Bytes> {
        if self.server.is_tcp {
            self.send_tcp(data).await
        } else {
            self.send_udp(data).await
        }
    }

    async fn connect_remote_server(&mut self) -> Result<()> {
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
                Ok(Err(e)) => {
                    bail!("connect {} failed. {e:?}", self.server.addr);
                }
                Err(_) => {
                    bail!("connect {} failed. timeout", self.server.addr);
                }
            }
        }

        if self.udp_socket.is_some() {
            return Ok(());
        }

        if let Ok(s) = UdpSocket::bind("0.0.0.0:0").await {
            if let Err(e) = s.connect(&self.server.addr).await {
                bail!("udp connect {} failed. {e:?}", self.server.addr);
            }
            self.udp_socket = s.into();
            return Ok(());
        }

        bail!("bind {} failed.", self.server.addr)
    }
}
