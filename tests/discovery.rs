//! The announcement, seen by a browser of our own — the same thing the phone
//! does, on loopback only.
//!
//! Ignored by default: multicast on a shared CI runner is not something to
//! gate a build on. Run it with
//! `cargo test --test discovery -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use nearscreen_receiver::net::discovery::SERVICE_TYPE;
use nearscreen_receiver::net::{Advertisement, Interfaces};

const PORT: u16 = 19913;
const PATIENCE: Duration = Duration::from_secs(10);

#[test]
#[ignore = "needs multicast on the loopback interface"]
fn a_browser_finds_the_receiver() {
    let advertisement =
        Advertisement::start("nearscreen test receiver", PORT, &Interfaces::Loopback).unwrap();
    println!("announced as {}", advertisement.fullname());

    let browser = ServiceDaemon::new().unwrap();
    browser.disable_interface(IfKind::All).unwrap();
    browser
        .enable_interface(vec![IfKind::LoopbackV4, IfKind::LoopbackV6])
        .unwrap();
    let found = browser.browse(SERVICE_TYPE).unwrap();

    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        let Ok(event) = found.recv_timeout(Duration::from_millis(500)) else {
            continue;
        };
        if let ServiceEvent::ServiceResolved(service) = event {
            println!(
                "found {} on {} port {} at {:?}",
                service.fullname, service.host, service.port, service.addresses
            );
            // Other receivers may well be on the same network; keep looking
            // until ours turns up.
            if !service.fullname.starts_with("nearscreen test receiver.") {
                continue;
            }
            assert_eq!(service.port, PORT);
            assert!(
                !service.addresses.is_empty(),
                "the announcement should carry an address to connect to"
            );
            return;
        }
    }
    panic!("the receiver did not show up on the network within {PATIENCE:?}");
}
