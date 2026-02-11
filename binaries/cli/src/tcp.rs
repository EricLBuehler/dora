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
        let reply_len = {
            let mut raw = [0; 8];
            self.stream.read_exact(&mut raw).await?;
            u64::from_le_bytes(raw) as usize
        };
        let mut reply = vec![0; reply_len];
        self.stream.read_exact(&mut reply).await?;
        Ok(reply)
    }
}
