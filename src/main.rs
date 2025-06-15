use async_trait::async_trait;
use dns_forward_over_tcp::server::DnsServer;
use dns_forward_over_tcp::server::RecordCallback;
use dns_forward_over_tcp::server::ServerInfo;
use getopts::Options;
use log::debug;
use std::env;
use std::error::Error;
use std::time::Instant;

fn print_usage(program: &str, opts: Options) {
    let brief = format!("Usage: {} [options]", program);
    print!("{}", opts.usage(&brief));
}

struct LogRecord {
    server: ServerInfo,
}

impl LogRecord {
    fn new(upstream: Option<&str>) -> LogRecord {
        if let Ok(s) = upstream.unwrap_or("tcp/8.8.8.8:53").parse::<ServerInfo>() {
            return LogRecord { server: s };
        }

        panic!("Invalid upstream server address. {:?}", upstream);
    }
}

#[async_trait]
impl RecordCallback<Instant> for LogRecord {
    async fn request(&self, res: &dns_parser::Packet<'_>) -> Option<(ServerInfo, Instant)> {
        for ele in &res.questions {
            debug!("res: {:?} {:?}", ele.qname, ele.qtype);
        }

        return Some((self.server.clone(), Instant::now()));
    }
    async fn response(&self, req: &dns_parser::Packet<'_>, res_time: Instant) {
        let mut req_name = String::new();
        for ele in &req.questions {
            req_name = ele.qname.to_string();
        }

        for ele in &req.answers {
            debug!(
                "req: {:?} {:?} {:?} {:?}",
                ele.name,
                req_name,
                ele.data,
                res_time.elapsed()
            );
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(not(debug_assertions))]
    simple_logger::init_with_level(log::Level::Info).unwrap();

    #[cfg(debug_assertions)]
    simple_logger::init_with_level(log::Level::Debug).unwrap();

    let args: Vec<String> = env::args().collect();
    let program = args[0].clone();

    let mut opts = Options::new();
    opts.optopt(
        "u",
        "upstream",
        "upstream server. default is 8.8.8.8:53",
        "IP:PORT",
    );
    opts.optopt("p", "", "listen port. default is :5353", "[IP]:PORT");
    opts.optflag("h", "help", "print this help menu");
    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(f) => {
            panic!("{}, please use -h for help", f.to_string())
        }
    };
    if matches.opt_present("h") {
        print_usage(&program, opts);
        return Ok(());
    }
    let port = matches.opt_str("p");
    let upstream = matches.opt_str("u");

    let s = DnsServer::new();
    if let Err(e) = s
        .run(port, None, Box::new(LogRecord::new(upstream.as_deref())))
        .await
    {
        panic!("Error running DNS server: {}", e);
    }

    Ok(())
}
