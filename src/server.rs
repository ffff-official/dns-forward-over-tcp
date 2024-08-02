use dns_parser::Packet;
use log::{error, warn};
use std::io::{Error, ErrorKind};
use std::process::ExitCode;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

pub type PreCallback = fn(&Packet, Option<&Packet>) -> bool;
pub type PostCallback = fn(&Packet, Option<&Packet>);

pub struct DnsServer {
    udp_server: UdpSocket,
    tcp_server: Option<TcpStream>,

    pre_foward_callback: Option<PreCallback>,
    post_foward_callback: Option<PostCallback>,

    upstream: String,
}

impl DnsServer {
    pub async fn run(
        port: Option<String>,
        upstream: Option<String>,
        pre_foward_callback: Option<PreCallback>,
        post_foward_callback: Option<PostCallback>,
    ) -> ExitCode {
        let bind_with_port = if let Some(port) = port {
            if port.contains(":") {
                port
            } else {
                String::from(format!("0.0.0.0:{}", port))
            }
        } else {
            String::from("127.0.0.1:5353")
        };

        let upstream = if let Some(upstream) = upstream {
            upstream
        } else {
            String::from("8.8.8.8:53")
        };

        let u = UdpSocket::bind(bind_with_port).await;
        if u.is_err() {
            error!("{}", u.err().unwrap().to_string());
            return ExitCode::FAILURE;
        }

        let mut s = DnsServer {
            upstream: upstream,
            tcp_server: None,
            udp_server: u.unwrap(),
            pre_foward_callback,
            post_foward_callback,
        };

        s._run().await;

        return ExitCode::SUCCESS;
    }

    async fn _run(&mut self) {
        loop {
            let mut buff = [0; 1024];

            let rr = self.udp_server.recv_from(&mut buff).await;
            if rr.is_err() {
                warn!("udp recv error.");
                continue;
            }

            let (packet_size, src_addr) = rr.unwrap();

            loop {
                let record = self.forward(&buff[..packet_size]).await;
                if record.is_err() {
                    let err = record.err().unwrap();

                    match err.kind() {
                        ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof => {
                            self.tcp_server = None;
                        }
                        _ => {}
                    }

                    warn!("{}", err.to_string());
                    continue;
                }

                let _ = self
                    .udp_server
                    .send_to(record.unwrap().as_ref(), src_addr)
                    .await;

                break;
            }
        }
    }

    async fn connect_remote_server(&mut self) {
        loop {
            if let Ok(s) = TcpStream::connect(&self.upstream).await {
                self.tcp_server = Some(s);
                return;
            }

            warn!("connect {} failed. try agine later.", &self.upstream);
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    async fn forward(&mut self, data: &[u8]) -> core::result::Result<Vec<u8>, std::io::Error> {
        if self.tcp_server.is_none() {
            self.connect_remote_server().await;
        }

        let dns_res_packet = dns_parser::Packet::parse(&data);
        if dns_res_packet.is_err() {
            warn!(
                "parse dns packet failed. {:?}",
                dns_res_packet.as_ref().err()
            );
        }

        if let Some(pre_foward_callback) = self.pre_foward_callback {
            if let Ok(dns_res_packet) = &dns_res_packet {
                if !pre_foward_callback(dns_res_packet, None) {
                    if let Ok(record) = dns_parser::Builder::new_query(
                        dns_res_packet.header.id,
                        dns_res_packet.header.recursion_available,
                    )
                    .build()
                    {
                        return Ok(record);
                    }
                }
            }
        }

        let tcp_server = self.tcp_server.as_mut().unwrap();

        let size = data.len() as u16;
        let r = tcp_server.write(&size.to_be_bytes()).await?;

        if r < size_of::<u16>() {
            return Err(Error::new(
                ErrorKind::Other,
                format!("forward data failed. {}", size),
            ));
        }

        if let Ok(size) = tcp_server.write(data).await {
            if size < data.len() {
                return Err(Error::new(
                    ErrorKind::Other,
                    format!("forward data failed. {}", size),
                ));
            }
        }

        let size = tcp_server.read_u16().await?;

        let mut buff = vec![0 as u8; size as usize];
        let size = tcp_server.read_exact(&mut buff).await?;

        if size < size_of_val(&buff) {
            return Err(Error::new(ErrorKind::Other, "tcp read data failed."));
        }

        let dns_req_packet = dns_parser::Packet::parse(&buff);
        if dns_req_packet.is_err() {
            warn!(
                "parse dns packet failed. {:?}",
                dns_req_packet.as_ref().err()
            );
        }

        if let Some(post_foward_callback) = self.post_foward_callback {
            if let Ok(dns_res_packet) = &dns_res_packet {
                post_foward_callback(&dns_res_packet, dns_req_packet.as_ref().ok());
            }
        }

        return Ok(buff);
    }
}
