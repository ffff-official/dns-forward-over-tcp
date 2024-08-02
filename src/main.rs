use getopts::Options;
use log::info;
use std::env;
use std::process::ExitCode;

use dns_forward_over_tcp::server::DnsServer;

fn print_usage(program: &str, opts: Options) {
    let brief = format!("Usage: {} [options]", program);
    print!("{}", opts.usage(&brief));
}

#[tokio::main]
async fn work(bind: Option<String>, upstream: Option<String>) -> ExitCode {
    return DnsServer::run(
        bind,
        upstream,
        Some(move |res, _| {
            for ele in &res.questions {
                info!("res: {:?} {:?}", ele.qname, ele.qtype);
            }
            return true;
        }),
        Some(|res, req_wrap| {
            if let Some(req) = req_wrap {
                for ele in &req.answers {
                    info!("req: {:?} {:?}", ele.name, ele.data);
                }
            }
        }),
    )
    .await;
}

fn main() -> ExitCode {
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
    opts.optflag("h", "help", "print this help menu");
    // opts.optflag("p", "", "listen port. default is 5353");
    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(f) => {
            panic!("{}, please use -h for help", f.to_string())
        }
    };
    if matches.opt_present("h") {
        print_usage(&program, opts);
        return ExitCode::FAILURE;
    }
    let port = matches.opt_str("p");
    let upstream = matches.opt_str("u");
    // let input = if !matches.free.is_empty() {
    //     matches.free[0].clone()
    // } else {
    //     print_usage(&program, opts);
    //     return;
    // };

    return work(port, upstream);
}
