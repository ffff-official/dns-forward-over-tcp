use async_trait::async_trait;
use dns_parser::Packet;
use flume::unbounded;
use log::{debug, error, warn};
use std::cmp;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::RwLock;

struct Forwarder {
    server: ServerInfo,

    udp_socket: Option<UdpSocket>,
    tcp_socket: Option<TcpStream>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub addr: SocketAddr,
    pub is_tcp: bool,
}

impl FromStr for ServerInfo {
    type Err = Box<dyn std::error::Error + Send + Sync>;

    fn from_str(server: &str) -> Result<Self, Self::Err> {
        let mut is_tcp = false;

        let s = server.split("/").collect::<Vec<&str>>();
        let server_only = if s.len() == 2 {
            if s[0] == "tcp" {
                is_tcp = true;
            }

            s[1]
        } else if s.len() == 1 {
            s[0]
        } else {
            return Err(format!("invalid server format: {}", server).into());
        };

        let s = server_only.split(":").collect::<Vec<&str>>();
        let server_with_port = if s.len() == 1 {
            format!("{}:53", s[0]).to_string()
        } else {
            server_only.to_string()
        };

        Ok(ServerInfo {
            is_tcp: is_tcp,
            addr: SocketAddr::from_str(&server_with_port)?,
        })
    }
    //
}

#[async_trait]
pub trait RecordCallback<T>: Send + Sync {
    async fn request(&self, res: &Packet<'_>) -> Option<(ServerInfo, T)>;
    async fn response(&self, req: &Packet<'_>, context: T);
}

impl Forwarder {
    pub fn new(server: ServerInfo) -> Self {
        Forwarder {
            server,
            udp_socket: None,
            tcp_socket: None,
        }
    }

    async fn send(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        static TIME_OUT: tokio::time::Duration = tokio::time::Duration::from_secs(3);

        loop {
            if self.server.is_tcp {
                if self.tcp_socket.is_none() {
                    self.connect_remote_server().await;
                }

                let tcp_server = self.tcp_socket.as_mut().unwrap();

                let size = data.len() as u16;
                match tcp_server.write(&size.to_be_bytes()).await {
                    Ok(r) => {
                        if r < size_of::<u16>() {
                            warn!("forward data failed. {}", size);
                            self.tcp_socket = None;
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
                        continue;
                    }
                }

                match tcp_server.write(data).await {
                    Ok(r) => {
                        if r < data.len() {
                            warn!("forward data failed. {}", size);
                            self.tcp_socket = None;
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
                        continue;
                    }
                    Err(_) => {
                        return Err("tcp read size timeout.".into());
                    }
                };

                let mut buff = vec![0 as u8; size as usize];
                match tokio::time::timeout(TIME_OUT, tcp_server.read_exact(&mut buff)).await {
                    Ok(Ok(s)) => {
                        if usize::from(s) < size_of_val(&buff) {
                            warn!("tcp read data failed.");
                            self.tcp_socket = None;
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
                        continue;
                    }
                    Err(_) => {
                        return Err("tcp read data timeout.".into());
                    }
                }

                return Ok(buff);
            }

            if self.udp_socket.is_none() {
                self.connect_remote_server().await;
            }

            let udp_socket = self.udp_socket.as_mut().unwrap();
            match udp_socket.send(&data).await {
                Err(e) => {
                    self.udp_socket = None;
                    return Err(e.into());
                }
                Ok(size) => {
                    if size < data.len() {
                        self.udp_socket = None;
                        return Err("udp send data failed.".into());
                    }
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
                    self.udp_socket = None;
                    return Err("udp recv data timeout.".into());
                }
            }
        }
    }

    async fn connect_remote_server(&mut self) {
        loop {
            if self.server.is_tcp {
                if let Ok(s) = TcpStream::connect(&self.server.addr).await {
                    self.tcp_socket = Some(s);
                    break;
                }

                warn!("connect {} failed. try again later.", self.server.addr);
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }

            if let Ok(s) = UdpSocket::bind("0.0.0.0:0").await {
                if let Err(e) = s.connect(&self.server.addr).await {
                    warn!("connect {} failed. {}", self.server.addr, e);
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                self.udp_socket = s.into();
                break;
            }

            warn!("bind {} failed. try again later.", self.server.addr);
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
    }
}

#[derive(Clone)]
pub struct DnsServer {
    fowarders: Arc<RwLock<Vec<Arc<RwLock<Forwarder>>>>>,
}

impl DnsServer {
    pub fn new() -> Self {
        DnsServer {
            fowarders: Arc::new(RwLock::new(vec![])),
        }
    }

    pub async fn run<T: 'static + Sync + Send>(
        &self,
        port: Option<String>,
        thread_num: Option<usize>,
        callback: Box<dyn RecordCallback<T>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bind_with_port = if let Some(port) = port {
            if port.contains(":") {
                port
            } else {
                String::from(format!("0.0.0.0:{}", port))
            }
        } else {
            String::from("127.0.0.1:5353")
        };

        let thread_num = if let Some(thread_num) = thread_num {
            cmp::min(thread_num, num_cpus::get())
        } else {
            cmp::min(2, num_cpus::get())
        };

        let callback = Arc::new(callback);
        let udp_socket = Arc::new(UdpSocket::bind(bind_with_port).await?);
        let server = Arc::new(self.clone());

        let (sender, receiver) = unbounded();
        let mut handles = vec![];

        for _ in 0..std::cmp::max(thread_num / 2, 1) {
            let udp_server = udp_socket.clone();
            let sender = sender.clone();

            handles.push(tokio::spawn(async move {
                loop {
                    let mut buff = [0; 1024];
                    let rr = udp_server.recv_from(&mut buff).await;
                    if rr.is_err() {
                        warn!("udp recv error. {:?}", rr.err());
                        continue;
                    }

                    if let Some((size, src_addr)) = rr.ok() {
                        let _ = sender.send_async((buff[..size].to_vec(), src_addr)).await;
                    }
                }
            }));
        }

        for _ in 0..thread_num {
            let callback = callback.clone();
            let receiver = receiver.clone();
            let server = server.clone();
            let reply = udp_socket.clone();

            handles.push(tokio::spawn(async move {
                loop {
                    if let Ok((buff, src_addr)) = receiver.recv_async().await {
                        server
                            .process(&buff, reply.clone(), src_addr, callback.clone())
                            .await;
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        Ok(())
    }

    async fn process<T>(
        &self,
        buff: &[u8],
        reply: Arc<UdpSocket>,
        src_addr: SocketAddr,
        callback: Arc<Box<dyn RecordCallback<T>>>,
    ) {
        let mut res_context = None;
        let fowarder = match dns_parser::Packet::parse(&buff) {
            Ok(dns_res_packet) => {
                let upstream = callback.request(&dns_res_packet).await;
                if upstream.is_none() {
                    if let Ok(record) = dns_parser::Builder::new_query(
                        dns_res_packet.header.id,
                        dns_res_packet.header.recursion_available,
                    )
                    .build()
                    {
                        let _ = reply.send_to(&record, src_addr).await;
                    }

                    return;
                }

                let (server, res_context2) = upstream.unwrap();
                res_context = res_context2.into();

                self.get_forwarder(Some(&server)).await.ok()
            }
            Err(e) => {
                warn!("process parse dns packet failed. {}", e);

                self.get_forwarder(None).await.ok()
            }
        }
        .unwrap();

        let mut fowarder = fowarder.write().await;
        let req_buff = fowarder.send(&buff).await;
        drop(fowarder);
        if req_buff.is_err() {
            error!(
                "forward dns request failed. {}",
                req_buff.as_ref().err().unwrap()
            );
            return;
        }

        let req_buff = req_buff.unwrap();

        match dns_parser::Packet::parse(&req_buff) {
            Ok(dns_req_packet) => {
                callback
                    .response(&dns_req_packet, res_context.unwrap())
                    .await;
            }
            Err(e) => {
                warn!("parse dns packet failed. {}", e);
            }
        }

        let _ = reply.send_to(&req_buff, src_addr).await;
    }

    async fn get_forwarder(
        &self,
        server: Option<&ServerInfo>,
    ) -> Result<Arc<RwLock<Forwarder>>, Box<dyn std::error::Error + Send + Sync>> {
        let fowarders = self.fowarders.read().await;
        debug!("get_forwarder: {:?}, size: {}", server, fowarders.len());

        for n in fowarders.iter() {
            if let Ok(m) = n.try_write() {
                if server.is_none() {
                    return Ok(n.clone());
                }

                if server == Some(&m.server) {
                    return Ok(n.clone());
                }
            }
        }
        drop(fowarders);

        if server.is_none() {
            return Err("No server provided to get_forwarder".into());
        }

        let server = server.unwrap();

        let mut fowarders = self.fowarders.write().await;
        let forward = Arc::new(RwLock::new(Forwarder::new(server.to_owned())));
        fowarders.push(forward.clone());

        Ok(forward)
    }
}
