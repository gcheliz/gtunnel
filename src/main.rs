use clap::Parser;

use gtunnel::cli::Cli;
use gtunnel::dispatch;

#[tokio::main]
async fn main() {
    // Before anything prints, so a reader that stops early ends every command
    // the same way
    dispatch::exit_quietly_on_broken_pipe();
    // Then, so a completion request is answered before anything can write
    // to stdout or load a config
    dispatch::answer_if_asked();
    let args = Cli::parse();
    let json = args.wants_json();
    if let Err(e) = dispatch::run(args).await {
        dispatch::exit_with(&e, json);
    }
    dispatch::finish();
}
