use crate::xor::header_xor_feedback;

pub struct ChannelKeys {
    pub remote_x: [u8; 32],
    pub local_x: [u8; 32],
    pub send_nonce: [u8; 24],
    pub recv_nonce: [u8; 24],
    pub send_hdr: u32,
    pub recv_hdr: u32,
}

pub fn increment_nonce(n: &mut [u8; 24]) {
    for chunk in n.chunks_mut(4) {
        let mut x = u32::from_le_bytes(chunk.try_into().unwrap());
        x = x.wrapping_add(1);
        chunk.copy_from_slice(&x.to_le_bytes());
        if x != 0 {
            break;
        }
    }
}

fn derive_nonces(nk: u32, session_ed_pk: &[u8; 32]) -> ([u8; 24], [u8; 24]) {
    // Client: receiver[j] = feedback(nk-42); sender[j] = receiver ^ 0x57575757
    let mut nk2 = nk.wrapping_sub(42) ^ u32::from_le_bytes(session_ed_pk[7..11].try_into().unwrap());
    let mut recv = [0u8; 24]; // client receiver = Prime sender
    let mut send = [0u8; 24]; // client sender = Prime receiver
    for j in (0..24).step_by(4) {
        let f = header_xor_feedback(nk2.wrapping_sub(42));
        recv[j..j + 4].copy_from_slice(&f.to_le_bytes());
        let s = f ^ 0x57575757;
        send[j..j + 4].copy_from_slice(&s.to_le_bytes());
        nk2 = !f;
    }
    (send, recv)
}

impl ChannelKeys {
    /// Prime side after a client hello.
    pub fn for_prime_after_hello(
        nk: u32,
        client_session_ed_pk: &[u8; 32],
        client_x_pk: &[u8; 32],
        server_x_sk: &[u8; 32],
    ) -> Self {
        let (client_send, client_recv) = derive_nonces(nk, client_session_ed_pk);
        Self {
            remote_x: *client_x_pk,
            local_x: *server_x_sk,
            send_nonce: client_recv,
            recv_nonce: client_send,
            send_hdr: header_xor_feedback(!nk),
            recv_hdr: header_xor_feedback(nk),
        }
    }

    /// Client side after sending hello (precomp filled after handshake with pool session keys).
    pub fn for_client_after_hello(nk: u32, session_ed_pk: &[u8; 32]) -> Self {
        let (send, recv) = derive_nonces(nk, session_ed_pk);
        Self {
            remote_x: [0u8; 32],
            local_x: [0u8; 32],
            send_nonce: send,
            recv_nonce: recv,
            send_hdr: header_xor_feedback(nk),
            recv_hdr: header_xor_feedback(!nk),
        }
    }

    pub fn set_precomp(&mut self, remote_x_pk: &[u8; 32], local_x_sk: &[u8; 32]) {
        self.remote_x = *remote_x_pk;
        self.local_x = *local_x_sk;
    }

    pub fn seal_channel(&mut self, plain: &[u8]) -> Vec<u8> {
        let out = crate::nacl::box_easy_afternm(plain, &self.send_nonce, &self.remote_x, &self.local_x)
            .unwrap_or_default();
        increment_nonce(&mut self.send_nonce);
        out
    }

    pub fn open_channel(&mut self, cipher: &[u8]) -> Result<Vec<u8>, ()> {
        let out = crate::nacl::box_open_easy_afternm(cipher, &self.recv_nonce, &self.remote_x, &self.local_x)?;
        increment_nonce(&mut self.recv_nonce);
        Ok(out)
    }

    pub fn next_send_hdr(&mut self) -> u32 {
        let k = self.send_hdr;
        self.send_hdr = header_xor_feedback(k);
        k
    }

    pub fn next_recv_hdr(&mut self) -> u32 {
        let k = self.recv_hdr;
        self.recv_hdr = header_xor_feedback(k);
        k
    }
}
