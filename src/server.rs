use async_trait::async_trait;
use crossbeam_channel::{bounded, unbounded, Receiver};
use dns_parser::Packet;
use log::{error, info, warn};
use std::cmp;
use std::io::{Error, ErrorKind};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::runtime::Runtime;

#[async_trait]
pub trait RecordCallback<T>: Send + Sync {
    async fn request(&self, res: &Packet<'_>) -> (bool, Option<T>);
    async fn response(&self, req: Option<&Packet<'_>>, context: Option<T>);
}

pub struct DnsServer<T> {
    udp_socket: Arc<UdpSocket>,
    tcp_server: Option<TcpStream>,

    upstream: String,
    callback: Arc<Box<dyn RecordCallback<T>>>,
}

impl<T: 'static + std::marker::Send> DnsServer<T> {
    pub async fn run(
        port: Option<String>,
        upstream: Option<String>,
        thread_num: Option<usize>,
        callback: Box<dyn RecordCallback<T>>,
    ) -> Result<(), Error> {
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

        let udp_socket = UdpSocket::bind(bind_with_port).await?;

        let udp_server = Arc::new(udp_socket);
        let udp_socket = udp_server.clone();

        let (sender, receiver) = unbounded();

        tokio::spawn(async move {
            loop {
                let mut buff = [0; 1024];
                let rr = udp_server.recv_from(&mut buff).await;
                if rr.is_err() {
                    warn!("udp recv error. {:?}", rr.err());
                    continue;
                }
                if let Some((size, src_addr)) = rr.ok() {
                    let _ = sender.send((buff[..size].to_vec(), src_addr));
                }
            }
        });

        let mut handles = vec![];
        let thread_num = if let Some(thread_num) = thread_num {
            cmp::min(thread_num, num_cpus::get())
        } else {
            cmp::min(4, num_cpus::get())
        };

        let runtime = Runtime::new().unwrap();
        let callback = Arc::new(callback);

        for _ in 0..thread_num {
            let udp_socket = udp_socket.clone();
            let receiver = receiver.clone();

            let callback = callback.clone();

            let mut s = DnsServer::<T> {
                udp_socket,
                upstream: upstream.clone(),

                tcp_server: None,
                callback,
            };

            handles.push(runtime.spawn(async move {
                Self::process(&mut s, receiver).await;
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        Ok(())
    }

    async fn process(dns_server: &mut DnsServer<T>, receiver: Receiver<(Vec<u8>, SocketAddr)>) {
        loop {
            let rr = receiver.recv();
            if rr.is_err() {
                continue;
            }

            let (buff, src_addr) = rr.ok().unwrap();

            let dns_res_packet = dns_parser::Packet::parse(&buff);
            if dns_res_packet.is_err() {
                warn!(
                    "parse dns packet failed. {:?}",
                    dns_res_packet.as_ref().err()
                );
            }

            let callback = dns_server.callback.clone();

            let mut res_context = None;

            if let Ok(dns_res_packet) = dns_res_packet {
                let (pass, context) = callback.request(&dns_res_packet).await;
                res_context = context;

                if !pass {
                    if let Ok(record) = dns_parser::Builder::new_query(
                        dns_res_packet.header.id,
                        dns_res_packet.header.recursion_available,
                    )
                    .build()
                    {
                        let _ = dns_server.udp_socket.send_to(&record, src_addr).await;
                    }

                    continue;
                }
            }

            loop {
                let req_buff = dns_server.forward(&buff).await;

                if req_buff.is_err() {
                    let err = req_buff.err().unwrap();

                    match err.kind() {
                        ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof => {}
                        _ => {
                            warn!("{}", err.to_string());
                        }
                    }

                    dns_server.tcp_server = None;
                    continue;
                }

                let req_buff = req_buff.ok().unwrap();

                let dns_req_packet = dns_parser::Packet::parse(&req_buff);
                if dns_req_packet.is_err() {
                    warn!(
                        "parse dns packet failed. {:?}",
                        dns_req_packet.as_ref().err()
                    );
                }

                callback
                    .response(dns_req_packet.ok().as_ref(), res_context)
                    .await;

                let _ = dns_server.udp_socket.send_to(&req_buff, src_addr).await;
                break;
            }
        }
    }

    async fn forward(&mut self, data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        if self.tcp_server.is_none() {
            self.connect_remote_server().await;
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

        let r = tcp_server.write(data).await?;
        if r < data.len() {
            return Err(Error::new(
                ErrorKind::Other,
                format!("forward data failed. {}", size),
            ));
        }

        let size = tcp_server.read_u16().await?;

        let mut buff = vec![0 as u8; size as usize];
        let size = tcp_server.read_exact(&mut buff).await?;

        if size < size_of_val(&buff) {
            return Err(Error::new(ErrorKind::Other, "tcp read data failed."));
        }

        return Ok(buff);
    }

    async fn connect_remote_server(&mut self) {
        loop {
            if let Ok(s) = TcpStream::connect(&self.upstream).await {
                self.tcp_server = Some(s);
                break;
            }

            warn!("connect {} failed. try again later.", &self.upstream);
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
