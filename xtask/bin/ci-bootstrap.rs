use std::{env, process};

use xtask::ci::{bootstrap, github};

fn main() {
    let mut args = env::args_os().skip(1).peekable();
    if args.peek().is_some_and(|mode| mode == "ci") {
        args.next();
        if args.next().is_none_or(|mode| mode != "cache")
            || args.next().is_none_or(|mode| mode != "record")
        {
            eprintln!("expected ci cache record");
            process::exit(1);
        }
        let Some(phase) = args.next() else {
            eprintln!("missing cache evidence phase");
            process::exit(1);
        };
        if args.next().is_some() {
            eprintln!("unexpected cache evidence argument");
            process::exit(1);
        }
        match bootstrap::record_evidence(&phase) {
            Ok(()) => process::exit(0),
            Err(error) => {
                eprintln!("cache evidence failed: {error}");
                process::exit(1);
            }
        }
    }
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
