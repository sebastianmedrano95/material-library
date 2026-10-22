mod mcp;
mod server;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = if arguments
        .first()
        .is_some_and(|argument| argument == "--mcp")
    {
        mcp::run(&arguments[1..])
    } else {
        server::run()
    };
    if let Err(error) = result {
        eprintln!("Material Library failed to start: {error}");
        std::process::exit(1);
    }
}
