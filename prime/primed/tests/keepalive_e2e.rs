//! End-to-end: a gateway the Prime has nothing to say to is sent a keep-alive it can accept.
//!
//! Ratum closes the session on a frame from the pool that is neither channel-encrypted nor
//! sealed, and the C gateways give up on a server they have not heard from in 60 s. So the
//! keep-alive is an empty ping on the session channel, and the session works after it. The
//! test waits out the 20 s keep-alive interval twice, so it takes about 45 s.

mod common;

use std::time::{Duration, Instant};

use common::*;
use datum_wire::cmd;
use datum_wire::crypto::Identity;

const TIP_BITS: u32 = 0x1903_a30c;
const TIP: [u8; 32] = [0x42; 32];

#[test]
fn an_idle_gateway_is_sent_an_empty_ping_on_the_channel_and_the_session_goes_on() {
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP),
        parent: node_hex(&[0x41; 32]),
        bits: TIP_BITS,
        next_bits: Some(TIP_BITS),
    });
    let pool = Identity::generate();
    let (primed, port) = start_primed(&pool, &node.config(0.2));
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats(&primed)["node"]["height"].as_u64().unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "primed never read the node's tip");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Ratum's hello: the gateway family that hangs up on a plain frame
    let (mut session, _configure) =
        Gateway::connect_as(port, &pool, &Identity::generate(), "ratum-gateway/0.1.66/ad2566e");
    // 20 s of nothing sent, then the next 5 s tick
    session.set_read_timeout(Duration::from_secs(30));
    for _ in 0..2 {
        let mut quiet_since = Instant::now();
        // `next_frame` panics on a frame that is not on the channel, and on one that does not open
        let (h, body) = loop {
            let (h, body) = session.next_frame();
            // the new-block notice a fresh session may be sent: not a keep-alive, and it puts one off
            if h.cmd != cmd::MINING {
                break (h, body);
            }
            quiet_since = Instant::now();
        };
        assert_eq!(h.cmd, cmd::PING, "{h:?}");
        assert!(!h.signed, "{h:?}");
        assert!(body.is_empty(), "a keep-alive carries nothing: {body:?}");
        let quiet = quiet_since.elapsed();
        assert!(quiet >= Duration::from_secs(15), "sent after {quiet:?} of quiet, not on the interval");
    }

    // both ends spent a nonce on each keep-alive: a request after them is still answered
    session.set_read_timeout(Duration::from_secs(10));
    session.request_coinbaser_seeing(VALUE, &TIP);
}
