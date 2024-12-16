use crate::Client;
use std::time::Duration;

impl Client {
    pub async fn announce_multicast(&self) -> crate::error::Result<()> {
        let msg = self.device.to_json()?;
        let addr = self.multicast_addr.clone();
        self.socket.send_to(msg.as_bytes(), addr)?;
        Ok(())
    }

    pub async fn listen_musticast(&self) -> crate::error::Result<()> {
        let mut buf = [0; 1024];

        loop {
            self.socket.set_read_timeout(Some(Duration::from_secs(5)))?;
            if let Err(e) = self.receive_message(&mut buf).await {
                eprintln!("Error receiving message: {}", e);
            }
        }
    }

    async fn receive_message(&self, buf: &mut [u8]) -> crate::error::Result<()> {
        match self.socket.recv_from(buf) {
            Ok((size, src)) => {
                let received_msg = String::from_utf8_lossy(&buf[..size]);
                self.process_device(&received_msg, src).await;
            }
            Err(e) => {
                return Err(e.into()); // Convert error to your crate's error type
            }
        }
        Ok(())
    }
}
