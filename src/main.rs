#![cfg_attr(not(test), windows_subsystem = "windows")]
#![cfg_attr(test, windows_subsystem = "console")]

mod backend;
mod background;
mod compact;
mod compression;
mod config;
mod console;
mod folder;
mod gui;
mod persistence;

fn short_sha() -> &'static str {
    match option_env!("VERGEN_GIT_SHA") {
        Some(sha) => sha.get(..8).unwrap_or(sha),
        None => "unknown",
    }
}

fn setup_panic() {
    std::panic::set_hook(Box::new(|panic_info| {
        if !console::alloc() {
            // No point trying to print without a console...
            return;
        }

        println!(
            r"
Oh dear, {app} has crashed.  Sorry :(

You can report this on the website at {website}/issues

Please try to include everything below the line, and give some hints as to what
you were doing - like what folder you were running it on.

#############################################################################

App: {app}, Version: {ver}, Build Date: {date}, Hash: {hash}
",
            app = env!("CARGO_PKG_NAME"),
            website = env!("CARGO_PKG_HOMEPAGE"),
            ver = env!("CARGO_PKG_VERSION"),
            date = option_env!("VERGEN_BUILD_DATE").unwrap_or("unknown"),
            hash = short_sha()
        );

        if let Some(message) = panic_info.payload().downcast_ref::<&'static str>() {
            println!("panic: {message}");
        } else if let Some(message) = panic_info.payload().downcast_ref::<String>() {
            println!("panic: {message}");
        } else {
            println!("panic: [mysteriously lacks a string representation]");
        }

        println!("\nHit Enter to print the rest of the debug info.");

        let _ = std::io::stdin().read_line(&mut String::new());

        let backtrace = std::backtrace::Backtrace::force_capture();

        println!("\n{backtrace:?}\n");
        println!("Hit Enter to continue.");

        let _ = std::io::stdin().read_line(&mut String::new());
    }));
}

fn main() {
    setup_panic();
    console::attach();

    let ret = std::panic::catch_unwind(gui::run);
    console::free();

    if ret.is_err() {
        std::process::exit(1);
    }
}
