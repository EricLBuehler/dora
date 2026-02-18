use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct AsyncTcpConnection {
    pub stream: tokio::net::TcpStream,
}

impl AsyncTcpConnection {
    pub async fn send(&mut self, request: &[u8]) -> std::io::Result<()> {
        let len_raw = (request.len() as u64).to_le_bytes();
        self.stream.write_all(&len_raw).await?;
        self.stream.write_all(request).await?;
        Ok(())
    }

    pub async fn receive(&mut self) -> std::io::Result<Vec<u8>> {
        const MAX_MESSAGE_LEN: usize = 256 * 1024 * 1024; // 256 MiB

        let reply_len = {
            let mut raw = [0; 8];
            self.stream.read_exact(&mut raw).await?;
            u64::from_le_bytes(raw) as usize
        };
        if reply_len > MAX_MESSAGE_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("message length {reply_len} exceeds maximum of {MAX_MESSAGE_LEN} bytes"),
            ));
        }
        let mut reply = vec![0; reply_len];
        self.stream.read_exact(&mut reply).await?;
        Ok(reply)
    }
}
