use async_rustls::{TlsAcceptor, TlsConnector};
use lazy_static::lazy_static;
use rustls::internal::pemfile::{certs, rsa_private_keys};
use rustls::{ClientConfig, ServerConfig};
use smol::io::{copy, split};
use smol::net::{TcpListener, TcpStream};
use smol::prelude::*;
use std::io;
use std::io::{BufReader, Cursor};
use std::net::SocketAddr;
use std::sync::mpsc::channel;
use std::sync::Arc;

const CERT: &str = include_str!("end.cert");
const CHAIN: &str = include_str!("end.chain");
const RSA: &str = include_str!("end.rsa");

lazy_static! {
    static ref TEST_SERVER: (SocketAddr, &'static str, &'static str) = {
        let cert = certs(&mut BufReader::new(Cursor::new(CERT))).unwrap();
        let mut keys = rsa_private_keys(&mut BufReader::new(Cursor::new(RSA))).unwrap();

        let mut config = ServerConfig::new(rustls::NoClientAuth::new());
        config
            .set_single_cert(cert, keys.pop().unwrap())
            .expect("invalid key or certificate");
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let (send, recv) = channel();

        smol::spawn(async move {
            let done = async move {
                async move {
                    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
                    let listener = TcpListener::bind(&addr).await?;

                    send.send(listener.local_addr()?).unwrap();

                    while let Some(stream) = listener.incoming().try_next().await? {
                        let acceptor = acceptor.clone();
                        let fut = async move {
                            async move {
                                let stream = acceptor.accept(stream).await?;

                                let (mut reader, mut writer) = split(stream);
                                copy(&mut reader, &mut writer).await?;

                                Ok(()) as io::Result<()>
                            }
                            .await
                            .unwrap_or_else(|err| eprintln!("server: {:?}", err));
                        };

                        smol::spawn(fut).detach();
                    }
                    Ok(())
                }
                .await
                .unwrap_or_else(|err: io::Error| eprintln!("server: {:?}", err));
            };
            done.await;
        })
        .detach();

        let addr = recv.recv().unwrap();
        (addr, "testserver.com", CHAIN)
    };
}

fn start_server() -> &'static (SocketAddr, &'static str, &'static str) {
    &*TEST_SERVER
}

async fn start_client(addr: SocketAddr, domain: &str, config: Arc<ClientConfig>) -> io::Result<()> {
    const FILE: &[u8] = include_bytes!("../Cargo.toml");

    let domain = webpki::DNSNameRef::try_from_ascii_str(domain).unwrap();
    let config = TlsConnector::from(config);
    let mut buf = vec![0; FILE.len()];

    let stream = TcpStream::connect(&addr).await?;
    let mut stream = config.connect(domain, stream).await?;
    stream.write_all(FILE).await?;
    stream.flush().await?;
    stream.read_exact(&mut buf).await?;

    assert_eq!(buf, FILE);

    Ok(())
}

#[test]
fn pass() -> io::Result<()> {
    smol::block_on(async {
        let (addr, domain, chain) = start_server();

        // TODO: not sure how to resolve this right now but since
        // TcpStream::bind now returns a future it creates a race
        // condition until its ready sometimes.
        use std::time::*;
        smol::Timer::after(Duration::from_secs(1)).await;

        let mut config = ClientConfig::new();
        let mut chain = BufReader::new(Cursor::new(chain));
        config.root_store.add_pem_file(&mut chain).unwrap();
        let config = Arc::new(config);

        start_client(*addr, domain, config.clone()).await?;

        Ok(())
    })
}

#[test]
fn fail() -> io::Result<()> {
    smol::block_on(async {
        let (addr, domain, chain) = start_server();

        let mut config = ClientConfig::new();
        let mut chain = BufReader::new(Cursor::new(chain));
        config.root_store.add_pem_file(&mut chain).unwrap();
        let config = Arc::new(config);

        assert_ne!(domain, &"google.com");
        let ret = start_client(*addr, "google.com", config).await;
        assert!(ret.is_err());

        Ok(())
    })
}
