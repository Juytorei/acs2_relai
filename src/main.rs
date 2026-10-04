// Relais UDP symétrique, à lancer sur les DEUX machines.
//
// Sur chaque machine, il écoute les ports 5005 et 5006 (là où arrivent les
// broadcasts 255.255.255.255 de l'application) et applique une règle simple :
//
//   - paquet venant de l'IP Tailscale de l'autre machine
//       -> rediffusé en broadcast (255.255.255.255) sur le LAN local
//   - n'importe quel autre paquet (donc l'application locale)
//       -> envoyé à l'IP Tailscale de l'autre machine
//
// Usage : udp_relay_simple <IP_TAILSCALE_DE_L_AUTRE_MACHINE> [-v]
//
// Cargo.toml : socket2 = { version = "0.5", features = ["all"] }

use socket2::{Domain, Protocol, Socket, Type};
use std::env;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

const PORTS: [u16; 2] = [5005, 5006];

// Un broadcast émis par le relais est aussi reçu par son propre socket.
// Sans garde-fou il serait renvoyé vers l'autre machine, qui le rediffuserait,
// etc. On ignore donc un paquet identique à celui qu'on vient de rediffuser.
const ECHO_WINDOW: Duration = Duration::from_millis(200);

/// Socket UDP sur 0.0.0.0:port, partageable (SO_REUSEADDR / SO_REUSEPORT)
/// et autorisé à émettre en broadcast.
fn create_socket(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;

    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;

    socket.set_broadcast(true)?;
    socket.bind(&SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    Ok(socket.into())
}

fn relay(socket: UdpSocket, port: u16, remote: Ipv4Addr, verbose: bool) {
    let to_remote = SocketAddrV4::new(remote, port);
    let to_lan = SocketAddrV4::new(Ipv4Addr::BROADCAST, port);

    let mut buffer = [0u8; 65535];
    let mut last_rebroadcast: Option<(Vec<u8>, Instant)> = None;

    loop {
        let (size, source) = match socket.recv_from(&mut buffer) {
            Ok(r) => r,
            // Windows : erreur parasite après un ICMP "port unreachable".
            Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
            Err(e) => {
                eprintln!("[ERREUR] Réception UDP {} : {}", port, e);
                thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let data = &buffer[..size];

        if source.ip() == IpAddr::V4(remote) {
            // Tailscale -> LAN
            if verbose {
                println!(
                    "[TAILSCALE -> LAN] {} -> {} ({} octets)",
                    source, to_lan, size
                );
            }
            if let Err(e) = socket.send_to(data, to_lan) {
                eprintln!("[ERREUR] Broadcast UDP {} : {}", port, e);
            }
            last_rebroadcast = Some((data.to_vec(), Instant::now()));
        } else {
            // Écho de notre propre broadcast : on ignore.
            if let Some((payload, when)) = &last_rebroadcast {
                if when.elapsed() < ECHO_WINDOW && payload.as_slice() == data {
                    continue;
                }
            }

            // LAN -> Tailscale
            if verbose {
                println!(
                    "[LAN -> TAILSCALE] {} -> {} ({} octets)",
                    source, to_remote, size
                );
            }
            if let Err(e) = socket.send_to(data, to_remote) {
                eprintln!("[ERREUR] Envoi vers {} : {}", to_remote, e);
            }
        }
    }
}

fn main() {
    let mut verbose = false;
    let mut remote_arg: Option<String> = None;

    for arg in env::args().skip(1) {
        if arg == "-v" || arg == "--verbose" {
            eprintln!("Verbose ON");
            verbose = true;
        } else {
            remote_arg = Some(arg);
        }
    }

    let remote_arg = remote_arg.unwrap_or_else(|| {
        eprintln!("Usage : udp_relay_simple <IP_TAILSCALE_DE_L_AUTRE_MACHINE> [-v]");
        process::exit(1);
    });

    let remote: Ipv4Addr = remote_arg.parse().unwrap_or_else(|_| {
        eprintln!("[ERREUR] Adresse IP invalide : {}", remote_arg);
        process::exit(1);
    });

    for port in PORTS {
        let socket = create_socket(port).unwrap_or_else(|e| {
            eprintln!("[ERREUR] Impossible d'écouter le port UDP {} : {}", port, e);
            process::exit(1);
        });

        thread::spawn(move || relay(socket, port, remote, verbose));
    }

    println!(
        "Relais actif : UDP {:?} <-> {} (Ctrl+C pour arrêter)",
        PORTS, remote
    );

    loop {
        thread::park();
    }
}
