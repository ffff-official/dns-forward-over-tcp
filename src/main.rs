use async_trait::async_trait;
use dns_forward_over_tcp::server::DnsServer;
use dns_forward_over_tcp::server::RecordCallback;
use getopts::Options;
use log::info;
use std::env;
use std::error::Error;
use std::time::Instant;

fn print_usage(program: &str, opts: Options) {
    let brief = format!("Usage: {} [options]", program);
    print!("{}", opts.usage(&brief));
}

struct LogRecord {}

impl LogRecord {
    fn new() -> LogRecord {
        return LogRecord {};
    }
}

#[async_trait]
impl RecordCallback<Instant> for LogRecord {
    async fn request(&self, res: &dns_parser::Packet<'_>) -> (bool, Option<Instant>) {
        for ele in &res.questions {
            info!("res: {:?} {:?}", ele.qname, ele.qtype);
        }

        return (true, Some(Instant::now()));
    }
    async fn response(&self, req: Option<&dns_parser::Packet<'_>>, res_time: Option<Instant>) {
        let t = Instant::now() - res_time.unwrap();

        if let Some(req) = req {
            let mut req_name = String::new();
            for ele in &req.questions {
                req_name = ele.qname.to_string();
            }

            for ele in &req.answers {
                info!("req: {:?} {:?} {:?} {:?}", ele.name, req_name, ele.data, t);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    simple_logger::init_with_level(log::Level::Info).unwrap();

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
    opts.optopt("t", "thread", "thread num. default is 2", "NUM");
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
    let thread_num = if let Some(thread_num) = matches.opt_str("t") {
        thread_num.parse::<usize>().ok()
    } else {
        None
    };

    DnsServer::run(port, upstream, thread_num, Box::new(LogRecord::new())).await?;

    Ok(())
}
