use std::{env, process};

use xtask::ci::{bootstrap, github};

fn main() {
    let mut args = env::args_os().skip(1).peekable();
    if args.peek().is_some_and(|mode| mode == "github") {
        args.next();
        let mode = args.next();
        let Some(mode) = mode.as_deref().and_then(|mode| mode.to_str()) else {
            eprintln!("missing GitHub gate");
            process::exit(1);
        };
        if args.next().is_some() {
            eprintln!("unexpected GitHub gate argument");
            process::exit(1);
        }
        match github::run(mode) {
            Ok(()) => process::exit(0),
            Err(error) => {
                eprintln!("{error}");
                process::exit(1);
            }
        }
    }
    match bootstrap::run(args) {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("CI bootstrap failed: {error}");
            process::exit(1);
        }
    }
}
