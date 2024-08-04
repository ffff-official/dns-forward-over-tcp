use dns_forward_over_tcp::server::DnsServer;
use getopts::Options;
use log::info;
use std::env;
use std::process::ExitCode;
use std::time::Instant;

fn print_usage(program: &str, opts: Options) {
    let brief = format!("Usage: {} [options]", program);
    print!("{}", opts.usage(&brief));
}

#[tokio::main]
async fn work(
    bind: Option<String>,
    upstream: Option<String>,
    thread_num: Option<usize>,
) -> ExitCode {
    return DnsServer::run(
        bind,
        upstream,
        thread_num,
        Some(move |res| {
            for ele in &res.questions {
                info!("res: {:?} {:?}", ele.qname, ele.qtype);
            }

            let n = Instant::now();

            return (
                true,
                Some(Box::new(move |req| {
                    let t = Instant::now() - n;

                    if let Some(req) = req {
                        for ele in &req.answers {
                            info!("req: {:?} {:?} {:?}", ele.name, ele.data, t);
                        }
                    }
                })),
            );
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
    opts.optopt("t", "thread", "thread num. default is 4", "NUM");
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
    let thread_num = if let Some(thread_num) = matches.opt_str("t") {
        thread_num.parse::<usize>().ok()
    } else {
        None
    };
    // let input = if !matches.free.is_empty() {
    //     matches.free[0].clone()
    // } else {
    //     print_usage(&program, opts);
    //     return;
    // };

    return work(port, upstream, thread_num);
}
