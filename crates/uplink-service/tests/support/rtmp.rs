use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
pub type Peer = tokio_rustls::client::TlsStream<TcpStream>;
fn string(value: &str) -> Vec<u8> {
    let mut b = vec![2];
    b.extend((value.len() as u16).to_be_bytes());
    b.extend(value.as_bytes());
    b
}
fn number(value: f64) -> Vec<u8> {
    let mut b = vec![0];
    b.extend(value.to_be_bytes());
    b
}
pub async fn message(peer: &mut Peer, kind: u8, stream: u32, body: &[u8]) {
    let mut data = vec![3, 0, 0, 0];
    data.extend(&(body.len() as u32).to_be_bytes()[1..]);
    data.push(kind);
    data.extend(stream.to_le_bytes());
    for (index, chunk) in body.chunks(128).enumerate() {
        if index > 0 {
            data.push(0xc3);
        }
        data.extend(chunk);
    }
    peer.write_all(&data).await.unwrap();
    peer.flush().await.unwrap();
}
pub async fn publish(peer: &mut Peer, key: &str) {
    let mut hello = vec![0; 1537];
    hello[0] = 3;
    peer.write_all(&hello).await.unwrap();
    peer.flush().await.unwrap();
    let mut reply = vec![0; 3073];
    peer.read_exact(&mut reply).await.unwrap();
    peer.write_all(&reply[1..1537]).await.unwrap();
    peer.flush().await.unwrap();
    let mut connect = string("connect");
    connect.extend(number(1.0));
    connect.extend([3, 0, 3, b'a', b'p', b'p']);
    connect.extend(string("live"));
    connect.extend([0, 0, 9]);
    message(peer, 20, 0, &connect).await;
    let mut publish = string("publish");
    publish.extend(number(0.0));
    publish.push(5);
    publish.extend(string(key));
    publish.extend(string("live"));
    message(peer, 20, 1, &publish).await;
}
pub async fn stop(peer: &mut Peer) {
    let mut end = string("deleteStream");
    end.extend(number(0.0));
    end.push(5);
    end.extend(number(1.0));
    message(peer, 20, 1, &end).await;
}
