//! The headless workspace server (docs/remote.md): spawned per session as
//! `ssh <alias> ide-server --stdio`, serves one workspace over stdio, exits
//! with the connection. Install on the host with
//! `cargo install --path ide --bin ide-server`.

fn main() {
    let stdio = std::env::args().nth(1).as_deref() == Some("--stdio");
    if !stdio {
        eprintln!("usage: ide-server --stdio   (spawned by the ide client over ssh)");
        std::process::exit(2);
    }
    if let Err(err) = smol::block_on(ide::server::serve_stdio()) {
        eprintln!("ide-server: {err:#}");
        std::process::exit(1);
    }
}
