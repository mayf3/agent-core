use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

fn main() {
    let address = std::env::var("SERVICE_LISTEN_ADDR").expect("SERVICE_LISTEN_ADDR");
    let component = std::env::var("COMPONENT_ID").expect("COMPONENT_ID");
    let version = std::env::var("COMPONENT_VERSION").expect("COMPONENT_VERSION");
    let instance = std::env::var("SERVICE_INSTANCE_ID").expect("SERVICE_INSTANCE_ID");
    let listener = TcpListener::bind(address).expect("bind fixture service");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => respond(&mut stream, &component, &version, &instance),
            Err(_) => break,
        }
    }
}

fn respond(stream: &mut TcpStream, component: &str, version: &str, instance: &str) {
    let mut request = [0u8; 1024];
    let Ok(read) = stream.read(&mut request) else {
        return;
    };
    let healthy = request[..read].starts_with(b"GET /health HTTP/1.1");
    let (status, body) = if healthy {
        ("200 OK", br#"{"status":"ok"}"#.as_slice())
    } else {
        ("404 Not Found", br#"{"error":"not_found"}"#.as_slice())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-Agent-Core-Component: {component}\r\nX-Agent-Core-Version: {version}\r\nX-Agent-Core-Instance: {instance}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
}
