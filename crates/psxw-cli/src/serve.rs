//! `psxw serve [dir] [--port N]` (M5): a static file server with the
//! cross-origin isolation headers SharedArrayBuffer and Atomics.wait need
//! (COOP `same-origin`, COEP `require-corp`). Development only.

use anyhow::{Context as _, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "css" => "text/css",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

fn respond(mut s: TcpStream, root: &Path) -> Result<()> {
    let mut line = String::new();
    let mut reader = BufReader::new(s.try_clone()?);
    reader.read_line(&mut line)?;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    let file = root.join(rel);
    let inside = file.canonicalize().ok().zip(root.canonicalize().ok()).is_some_and(|(f, r)| f.starts_with(r));
    let (status, body, ty) = match std::fs::read(&file) {
        Ok(b) if inside => ("200 OK", b, content_type(&file)),
        _ => ("404 Not Found", b"not found".to_vec(), "text/plain"),
    };
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\nCross-Origin-Resource-Policy: same-origin\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    s.write_all(&body)?;
    Ok(())
}

pub fn serve(root: PathBuf, port: u16) -> Result<()> {
    let l = TcpListener::bind(("127.0.0.1", port)).with_context(|| format!("binding 127.0.0.1:{port}"))?;
    eprintln!("serving {} at http://127.0.0.1:{port}/ (COOP/COEP set)", root.display());
    for s in l.incoming() {
        let root = root.clone();
        let s = s?;
        std::thread::spawn(move || {
            let _ = respond(s, &root);
        });
    }
    Ok(())
}
