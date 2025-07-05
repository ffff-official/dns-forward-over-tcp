use async_trait::async_trait;
use dns_parser::Packet;
use flume::unbounded;
use log::{debug, error, warn};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{OnceCell, RwLock};

use crate::forward::Forwarder;

static DEFAULT_UPSTREAM: OnceCell<ServerInfo> = OnceCell::const_new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DNSPriority {
    Low = -1,
    Normal = 0,
    High = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub addr: SocketAddr,
    pub is_tcp: bool,

    pub priority: DNSPriority,
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
            priority: DNSPriority::Normal,
        })
    }
    //
}

impl std::fmt::Display for ServerInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_tcp {
            write!(f, "tcp/{}", self.addr)
        } else {
            write!(f, "udp/{}", self.addr)
        }
    }
}

#[async_trait]
pub trait RecordCallback<T>: Send + Sync {
    async fn request(&self, res: &Packet<'_>) -> Option<(ServerInfo, T)>;
    async fn response(&self, req: &Packet<'_>, context: T);
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
        default_upstream: Option<String>,
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

        let spwan_num = num_cpus::get();

        DEFAULT_UPSTREAM
            .get_or_init(|| async {
                default_upstream
                    .unwrap_or("tcp/8.8.8.8".into())
                    .parse()
                    .unwrap()
            })
            .await;
        let callback = Arc::new(callback);
        let udp_socket = Arc::new(UdpSocket::bind(bind_with_port).await?);
        let server = self;

        let (sender, receiver) = unbounded();
        let mut handles = vec![];

        {
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

        let (forward_sender, forward_receiver) =
            unbounded::<(Arc<RwLock<Forwarder>>, SocketAddr, Vec<u8>, Option<T>)>();

        for _ in 0..spwan_num {
            let callback = callback.clone();
            let receiver = receiver.clone();
            let server = server.clone();
            let reply = udp_socket.clone();
            let forward_sender = forward_sender.clone();

            handles.push(tokio::spawn(async move {
                loop {
                    if let Ok((buff, src_addr)) = receiver.recv_async().await {
                        server
                            .process(
                                &buff,
                                forward_sender.clone(),
                                reply.clone(),
                                src_addr,
                                callback.clone(),
                            )
                            .await;
                    }
                }
            }));
        }

        for _ in 0..spwan_num {
            let callback = callback.clone();
            let receiver = forward_receiver.clone();
            let server = server.clone();
            let reply = udp_socket.clone();

            handles.push(tokio::spawn(async move {
                loop {
                    if let Ok((fowarder, src_addr, buff, res_context)) = receiver.recv_async().await
                    {
                        server
                            .process_forward(
                                fowarder,
                                src_addr,
                                &buff,
                                reply.clone(),
                                callback.clone(),
                                res_context,
                            )
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
        forward_sender: flume::Sender<(Arc<RwLock<Forwarder>>, SocketAddr, Vec<u8>, Option<T>)>,
        reply: Arc<UdpSocket>,
        src_addr: SocketAddr,
        callback: Arc<Box<dyn RecordCallback<T>>>,
    ) {
        let mut res_context = None;
        let mut priority = DNSPriority::Normal;
        let fowarder = match dns_parser::Packet::parse(&buff) {
            Ok(dns_res_packet) => {
                let upstream = callback.request(&dns_res_packet).await;
                if upstream.is_none() {
                    let mut b = dns_parser::Builder::new_query(
                        dns_res_packet.header.id,
                        dns_res_packet.header.recursion_available,
                    );

                    if dns_res_packet.questions.len() > 0 {
                        let question = &dns_res_packet.questions[0];
                        if matches!(
                            question.qtype,
                            dns_parser::QueryType::A | dns_parser::QueryType::AAAA
                        ) {
                            b.add_question(
                                question.qname.to_string().as_str(),
                                question.prefer_unicast,
                                question.qtype,
                                question.qclass,
                            );

                            b.add_answer(
                                question.qname.to_string().as_str(),
                                3600,
                                std::net::Ipv4Addr::new(0, 0, 0, 0),
                            );

                            if let Ok(r) = b.build() {
                                let _ = reply.send_to(&r, src_addr).await;
                            }

                            return;
                        }
                    }

                    if let Ok(r) = b.build() {
                        //fixme:
                        let _ = reply.send_to(&r, src_addr).await;
                    }

                    return;
                }

                let (server, res_context2) = upstream.unwrap();
                priority = server.priority.clone();
                res_context = res_context2.into();

                self.get_forwarder(Some(&server)).await.ok()
            }
            Err(e) => {
                warn!("parse res dns packet failed. {}", e);

                self.get_forwarder(None).await.ok()
            }
        }
        .unwrap();

        if priority == DNSPriority::High {
            self.process_forward(
                fowarder,
                src_addr,
                &buff,
                reply.clone(),
                callback.clone(),
                res_context,
            )
            .await;
        } else {
            let _ = forward_sender
                .send_async((fowarder, src_addr, buff.to_vec(), res_context))
                .await;
        }
    }

    async fn process_forward<T>(
        &self,
        fowarder: Arc<RwLock<Forwarder>>,
        src_addr: SocketAddr,
        buff: &[u8],
        reply: Arc<UdpSocket>,
        callback: Arc<Box<dyn RecordCallback<T>>>,
        res_context: Option<T>,
    ) {
        let mut fowarder = fowarder.write().await;
        match fowarder.send(&buff).await {
            Ok(req_buff) => {
                match dns_parser::Packet::parse(&req_buff) {
                    Ok(dns_req_packet) => {
                        callback
                            .response(&dns_req_packet, res_context.unwrap())
                            .await;
                    }
                    Err(e) => {
                        warn!("parse req dns packet failed. {}", e);
                    }
                }

                let _ = reply.send_to(&req_buff, src_addr).await;
            }
            Err(e) => {
                let p = dns_parser::Packet::parse(&buff);

                error!("forward dns request failed. {:?}, {}", p, e);
            }
        }
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

        let server = server.unwrap_or(DEFAULT_UPSTREAM.get().unwrap());

        let mut fowarders = self.fowarders.write().await;
        let forward = Arc::new(RwLock::new(Forwarder::new(server.to_owned())));
        fowarders.push(forward.clone());

        Ok(forward)
    }

    pub async fn get_forwarders(&self) -> Vec<String> {
        let mut r = Vec::new();

        let fowarders = self.fowarders.read().await;
        for f in fowarders.iter() {
            let f = f.read().await;

            r.push(f.server.to_string());
        }

        return r;
    }
}
