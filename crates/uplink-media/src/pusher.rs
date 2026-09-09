//! Begrenzter RTMP(S)-Publisher. Pro Ziel genau eine abbrechbare Task.
//! Codec- und Trackkörper bleiben unverändert; Plattformrechte prüft der Adapter.
use crate::{
    MediaError, MediaLimits, OutputState, OutputStatus, PublishTarget, Result,
    flv::FlvTag,
    queue::{self, PacketReceiver, PacketSender},
};
use bytes::{Bytes, BytesMut};
use scuffle_amf0::{Amf0Decoder, Amf0Encoder, Amf0Value, DecodeLimits};
use scuffle_rtmp::{
    chunk::{
        Chunk,
        reader::{ChunkReader, ChunkReaderLimits},
        writer::ChunkWriter,
    },
    messages::MessageType,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, lookup_host},
    sync::watch,
    task::JoinHandle,
    time::{Instant, timeout, timeout_at},
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use url::Url;
use zeroize::Zeroizing;

const CONTROL_BYTES: usize = 64 * 1024;
const INPUT_BYTES: usize = 128 * 1024;
const COMMAND_LIMIT: usize = 4096;

/// Eigentümer der Zielverbindung. Drop und abgebrochenes finish terminieren sie.
pub struct RunningPusher {
    sender: Option<PacketSender>,
    status: watch::Receiver<OutputStatus>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<OutputStatus>>>,
    max_tag_bytes: usize,
    shutdown_timeout: Duration,
}
impl RunningPusher {
    /// Kehrt erst nach bestätigtem NetStream.Publish.Start zurück.
    pub async fn start(target: PublishTarget, limits: MediaLimits) -> Result<Self> {
        let mut pusher = Self::spawn(target, limits)?;
        pusher.wait_published().await?;
        Ok(pusher)
    }
    /// Die begrenzte Queue existiert vor DNS/TLS/RTMP. Kein fremder Handshake
    /// darf den Medienkonsum oder den Start eines anderen Ziels aufhalten.
    pub fn spawn(target: PublishTarget, limits: MediaLimits) -> Result<Self> {
        validate_limits(&limits)?;
        endpoint(&target)?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| MediaError::InvalidConfiguration)?;
        let (sender, receiver) = queue::bounded(&limits)?;
        let deadline = Instant::now() + limits.startup_timeout;
        let status = OutputStatus {
            id: target.id.clone(),
            state: OutputState::Starting,
            received_bytes: 0,
            received_events: 0,
        };
        let (updates, status) = watch::channel(status);
        let (stop, stop_rx) = watch::channel(false);
        let shutdown_timeout = limits.shutdown_timeout;
        let max_tag_bytes = limits.max_tag_bytes;
        let task = runtime.spawn(async move {
            let mut startup_stop = stop_rx.clone();
            let connected = {
                let startup = async {
                    let mut client = timeout_at(deadline, Client::connect(&target, &limits))
                        .await
                        .map_err(|_| MediaError::StartTimeout)??;
                    timeout_at(deadline, client.publish(&target))
                        .await
                        .map_err(|_| MediaError::StartTimeout)??;
                    Ok::<_, MediaError>(client)
                };
                if *stop_rx.borrow() {
                    Err(MediaError::Cancelled)
                } else {
                    tokio::select! {
                        biased;
                        _=startup_stop.changed()=>Err(MediaError::Cancelled),
                        result=startup=>result,
                    }
                }
            };
            // Der Publish-Zugang wird auch nach einem fehlgeschlagenen oder
            // abgebrochenen Start verworfen und nicht von der Medienphase gehalten.
            drop(target);
            let result = match connected {
                Ok(mut client) => {
                    updates.send_modify(|status| status.state = OutputState::Publishing);
                    client.run(receiver, stop_rx, &updates).await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                updates.send_modify(|status| status.state = OutputState::Failed(error));
            }
            result.map(|()| updates.borrow().clone())
        });
        Ok(Self {
            sender: Some(sender),
            status,
            stop,
            task: Some(task),
            max_tag_bytes,
            shutdown_timeout,
        })
    }
    pub async fn wait_published(&mut self) -> Result<()> {
        loop {
            match self.status.borrow().state {
                OutputState::Publishing => return Ok(()),
                OutputState::Failed(error) => return Err(error),
                OutputState::LocalEndUnconfirmed => return Err(MediaError::Cancelled),
                OutputState::Starting => {}
            }
            self.status
                .changed()
                .await
                .map_err(|_| MediaError::Cancelled)?;
        }
    }
    pub fn try_send(&self, tag: Arc<FlvTag>) -> Result<()> {
        if *self.stop.borrow() {
            return Err(MediaError::Cancelled);
        }
        if tag.body().len() > self.max_tag_bytes {
            return Err(MediaError::ResourceLimit);
        }
        if let OutputState::Failed(error) = self.status.borrow().state {
            return Err(error);
        }
        self.sender
            .as_ref()
            .ok_or(MediaError::Cancelled)?
            .try_send(tag)
    }
    pub fn status(&self) -> OutputStatus {
        self.status.borrow().clone()
    }
    /// Nichtblockierendes Stop-Signal; finish/stop übernehmen anschließend den Join.
    pub fn cancel(&self) {
        self.stop.send_replace(true);
    }
    pub(crate) fn cancellation(&self) -> watch::Sender<bool> {
        self.stop.clone()
    }
    /// Wartende Medien innerhalb der Abschlussfrist ausgeben, danach deleteStream.
    pub async fn finish(mut self) -> Result<OutputStatus> {
        self.sender.take();
        let task = self.task.as_mut().ok_or(MediaError::Cancelled)?;
        let report = match timeout(self.shutdown_timeout, task).await {
            Ok(result) => result.map_err(|_| MediaError::ProcessCleanupFailed)?,
            Err(_) => {
                let reason = if self.status.borrow().state == OutputState::Starting {
                    MediaError::StartTimeout
                } else {
                    MediaError::Io
                };
                self.cancel();
                let task = self.task.as_mut().ok_or(MediaError::Cancelled)?;
                task.abort();
                let joined = timeout(self.shutdown_timeout, task)
                    .await
                    .map_err(|_| MediaError::ProcessCleanupFailed)?;
                if joined.is_err_and(|error| !error.is_cancelled()) {
                    return Err(MediaError::ProcessCleanupFailed);
                }
                self.task.take();
                return Err(reason);
            }
        };
        self.task.take();
        report
    }
    /// Sofortiger expliziter Abbruch; wartet auf die Freigabe der Verbindung.
    pub async fn stop(mut self) -> Result<OutputStatus> {
        self.sender.take();
        self.cancel();
        let task = self.task.as_mut().ok_or(MediaError::Cancelled)?;
        let report = timeout(self.shutdown_timeout, task)
            .await
            .map_err(|_| MediaError::ProcessCleanupFailed)?
            .map_err(|_| MediaError::ProcessCleanupFailed)?;
        self.task.take();
        report
    }
}
impl Drop for RunningPusher {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

fn validate_limits(limits: &MediaLimits) -> Result<()> {
    if limits.max_tag_bytes == 0
        || limits.max_tag_bytes > 0xff_ffff
        || limits.queue_bytes < limits.max_tag_bytes + 15
        || [
            limits.startup_timeout,
            limits.write_timeout,
            limits.shutdown_timeout,
        ]
        .iter()
        .any(|d| d.is_zero() || *d > Duration::from_secs(300))
    {
        return Err(MediaError::InvalidConfiguration);
    }
    Ok(())
}

struct Endpoint {
    host: String,
    port: u16,
    app: String,
    tc_url: String,
    tls: bool,
}
fn endpoint(target: &PublishTarget) -> Result<Endpoint> {
    std::str::from_utf8(target.playpath.expose_for_pipe())
        .map_err(|_| MediaError::InvalidConfiguration)?;
    if target.endpoint.len() > 2048
        || target.id.is_empty()
        || target.id.len() > 128
        || target.allowed_hosts.len() > 64
    {
        return Err(MediaError::EndpointRejected);
    }
    let url = Url::parse(&target.endpoint).map_err(|_| MediaError::EndpointRejected)?;
    let tls = match url.scheme() {
        "rtmps" => true,
        "rtmp" if target.allow_unencrypted => false,
        _ => return Err(MediaError::EndpointRejected),
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MediaError::EndpointRejected);
    }
    let host = url
        .host_str()
        .ok_or(MediaError::EndpointRejected)?
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    if !target
        .allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&host))
    {
        return Err(MediaError::EndpointRejected);
    }
    let app = url.path().trim_matches('/');
    if app.is_empty()
        || app.len() > 512
        || app
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b"/-_.".contains(&b)))
    {
        return Err(MediaError::EndpointRejected);
    }
    Ok(Endpoint {
        host,
        port: url.port().unwrap_or(if tls { 443 } else { 1935 }),
        app: app.into(),
        tc_url: url.to_string(),
        tls,
    })
}

/// Fail-closed für lokale, reservierte und Transition-Netze. Testmodus erlaubt
/// ausschließlich Loopback zusätzlich; niemals sonstige private Netze.
fn permitted_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return allow_loopback;
    }
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return permitted_ip(IpAddr::V4(v4), allow_loopback);
            }
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

fn permitted_target_ip(ip: IpAddr, host: &str, allow_loopback: bool) -> bool {
    if allow_loopback && host == "localhost" {
        ip.is_loopback()
    } else {
        permitted_ip(ip, allow_loopback)
    }
}

trait Socket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Socket for T {}
struct Client {
    socket: Box<dyn Socket>,
    parser: ChunkReader,
    writer: ChunkWriter,
    input: BytesMut,
    stream_id: u32,
    received: u64,
    acknowledged: u64,
    window: u32,
    sent: u64,
    peer_acknowledged: u64,
    peer_window: Option<(u32, u8)>,
    outgoing_chunk_size: usize,
    write_timeout: Duration,
    end_observation: Duration,
    ending: bool,
    control_epoch: Instant,
    control_count: usize,
}
#[derive(PartialEq)]
enum Response {
    Other,
    Connected,
    Created(u32),
    Published,
    Unpublished,
}
impl Client {
    async fn connect(target: &PublishTarget, limits: &MediaLimits) -> Result<Self> {
        let endpoint = endpoint(target)?;
        let addresses: Vec<SocketAddr> = lookup_host((endpoint.host.as_str(), endpoint.port))
            .await
            .map_err(|_| MediaError::Io)?
            .take(17)
            .collect();
        if addresses.is_empty()
            || addresses.len() > 16
            || addresses
                .iter()
                .any(|a| !permitted_target_ip(a.ip(), &endpoint.host, target.allow_loopback))
        {
            return Err(MediaError::EndpointRejected);
        }
        // Genau die geprüften Adressen verbinden: keine zweite DNS-Auflösung.
        let mut connected = None;
        for address in addresses {
            if let Ok(Ok(socket)) = timeout(limits.write_timeout, TcpStream::connect(address)).await
            {
                connected = Some(socket);
                break;
            }
        }
        let tcp = connected.ok_or(MediaError::Io)?;
        tcp.set_nodelay(true).map_err(|_| MediaError::Io)?;
        let mut socket: Box<dyn Socket> = if endpoint.tls {
            let config = if let Some(config) = &target.tls {
                config.clone()
            } else {
                let roots =
                    RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                Arc::new(
                    ClientConfig::builder_with_provider(Arc::new(
                        tokio_rustls::rustls::crypto::ring::default_provider(),
                    ))
                    .with_safe_default_protocol_versions()
                    .map_err(|_| MediaError::TlsRejected)?
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
                )
            };
            let name =
                ServerName::try_from(endpoint.host.clone()).map_err(|_| MediaError::TlsRejected)?;
            Box::new(
                TlsConnector::from(config)
                    .connect(name, tcp)
                    .await
                    .map_err(|_| MediaError::TlsRejected)?,
            )
        } else {
            Box::new(tcp)
        };
        handshake(&mut socket).await?;
        let parser = ChunkReader::with_limits(ChunkReaderLimits {
            max_message_bytes: CONTROL_BYTES,
            max_command_bytes: CONTROL_BYTES,
            max_chunk_streams: 16,
            max_partial_messages: 4,
            max_partial_bytes: INPUT_BYTES,
            max_chunk_size: CONTROL_BYTES,
        })
        .map_err(|_| MediaError::InvalidConfiguration)?;
        Ok(Self {
            socket,
            parser,
            writer: ChunkWriter::default(),
            input: BytesMut::new(),
            stream_id: 0,
            received: 0,
            acknowledged: 0,
            window: 2_500_000,
            sent: 0,
            peer_acknowledged: 0,
            peer_window: None,
            outgoing_chunk_size: 128,
            write_timeout: limits.write_timeout,
            end_observation: Duration::from_millis(250)
                .min(limits.shutdown_timeout / 4)
                .min(limits.write_timeout),
            ending: false,
            control_epoch: Instant::now(),
            control_count: 0,
        })
    }
    async fn publish(&mut self, target: &PublishTarget) -> Result<()> {
        let endpoint = endpoint(target)?;
        let properties = [
            (
                "app".into(),
                Amf0Value::String(endpoint.app.as_str().into()),
            ),
            (
                "tcUrl".into(),
                Amf0Value::String(endpoint.tc_url.as_str().into()),
            ),
            (
                "flashVer".into(),
                Amf0Value::String("FMLE/3.0 (compatible; Uplink)".into()),
            ),
            ("fpad".into(), Amf0Value::Boolean(false)),
            ("capabilities".into(), Amf0Value::Number(15.0)),
            ("objectEncoding".into(), Amf0Value::Number(0.0)),
            ("capsEx".into(), Amf0Value::Number(2.0)),
            (
                "videoFourCcInfoMap".into(),
                Amf0Value::Object(
                    ["av01", "hvc1", "avc1"]
                        .into_iter()
                        .map(|codec| (codec.into(), Amf0Value::Number(4.0)))
                        .collect(),
                ),
            ),
            (
                "audioFourCcInfoMap".into(),
                Amf0Value::Object(
                    [("mp4a".into(), Amf0Value::Number(4.0))]
                        .into_iter()
                        .collect(),
                ),
            ),
            (
                "fourCcList".into(),
                Amf0Value::Array(
                    vec![
                        Amf0Value::String("av01".into()),
                        Amf0Value::String("hvc1".into()),
                        Amf0Value::String("avc1".into()),
                        Amf0Value::String("mp4a".into()),
                    ]
                    .into(),
                ),
            ),
        ]
        .into_iter()
        .collect();
        self.command("connect", 1.0, 0, &[Amf0Value::Object(properties)])
            .await?;
        while self.next_response().await? != Response::Connected {}
        self.control(1, &4096_u32.to_be_bytes()).await?;
        self.writer.set_chunk_size(4096);
        self.outgoing_chunk_size = 4096;
        self.command("createStream", 2.0, 0, &[Amf0Value::Null])
            .await?;
        loop {
            if let Response::Created(id) = self.next_response().await? {
                self.stream_id = id;
                break;
            }
        }
        let playpath = std::str::from_utf8(target.playpath.expose_for_pipe())
            .map_err(|_| MediaError::InvalidConfiguration)?;
        self.command(
            "publish",
            3.0,
            self.stream_id,
            &[
                Amf0Value::Null,
                Amf0Value::String(playpath.into()),
                Amf0Value::String("live".into()),
            ],
        )
        .await?;
        while self.next_response().await? != Response::Published {}
        Ok(())
    }
    async fn run(
        &mut self,
        mut receiver: PacketReceiver,
        mut stop: watch::Receiver<bool>,
        updates: &watch::Sender<OutputStatus>,
    ) -> Result<()> {
        let mut scratch = [0_u8; 16 * 1024];
        loop {
            if *stop.borrow() {
                return Err(MediaError::Cancelled);
            }
            // Verarbeitete Kontrollnachrichten werden nie durch einen abgebrochenen
            // Lese-Future verloren; nur read() selbst liegt im select.
            while let Some(chunk) = self
                .parser
                .read_chunk(&mut self.input)
                .map_err(|_| MediaError::ProtocolRejected)?
            {
                self.handle(chunk).await?;
            }
            self.ack_if_due().await?;
            tokio::select! {
                biased;
                _ = stop.changed() => return Err(MediaError::Cancelled),
                read = self.socket.read(&mut scratch) => {
                    let count = read.map_err(|_| MediaError::Io)?;
                    self.accept_read(&scratch[..count])?;
                }
                packet = receiver.recv() => {
                    let Some(packet) = packet else { break; };
                    let tag = packet.tag();
                    let csid = match tag.kind() { 8 => 4, 9 => 5, _ => 6 };
                    self.write(csid, tag.timestamp_ms(), tag.kind(), self.stream_id, Bytes::copy_from_slice(tag.body())).await?;
                    updates.send_modify(|status| { status.received_events = status.received_events.saturating_add(1); status.received_bytes = status.received_bytes.saturating_add(tag.body().len() as u64); });
                }
            }
        }
        // Die Queue kann im selben Poll enden, in dem die Gegenstelle noch
        // eine Abweisung sendet. Vor dem Stop auch verzögerte Kontrollen lesen.
        // Diese begrenzte Beobachtung ist ausdrücklich keine Empfangsbarriere.
        self.observe_end_controls(&mut stop).await?;
        self.ending = true;
        self.command(
            "deleteStream",
            0.0,
            0,
            &[Amf0Value::Null, Amf0Value::Number(self.stream_id as f64)],
        )
        .await?;
        let peer_closed = self.observe_end_controls(&mut stop).await?;
        if !peer_closed {
            timeout(self.write_timeout, self.socket.shutdown())
                .await
                .map_err(|_| MediaError::Io)?
                .map_err(|_| MediaError::Io)?;
        }
        if *stop.borrow() {
            return Err(MediaError::Cancelled);
        }
        updates.send_modify(|status| status.state = OutputState::LocalEndUnconfirmed);
        Ok(())
    }
    /// deleteStream hat laut RTMP 7.2.2.3 keine Serverantwort. Weder ein EOF,
    /// Unpublish.Success noch das Ausbleiben weiterer Daten bestätigt Medien
    /// oder Veröffentlichung. Explizite Fehler bleiben trotzdem terminal.
    /// Nach unserem Stop ist ein Transportende mehrdeutig; davor ist es ein
    /// unerwarteter Verbindungsabbruch. Deadline gilt je Phase, nie je Nachricht.
    async fn observe_end_controls(&mut self, stop: &mut watch::Receiver<bool>) -> Result<bool> {
        let deadline = Instant::now() + self.end_observation;
        let mut scratch = [0; 16 * 1024];
        loop {
            if *stop.borrow() {
                return Err(MediaError::Cancelled);
            }
            // Parserzustand bleibt über jeden read und beide Phasen erhalten.
            // handle()/write_all() werden nicht durch einen read-Timer verworfen.
            while let Some(chunk) = self
                .parser
                .read_chunk(&mut self.input)
                .map_err(|_| MediaError::ProtocolRejected)?
            {
                self.handle(chunk).await?;
            }
            self.ack_if_due().await?;
            let read = tokio::select! {
                biased;
                _ = stop.changed() => return Err(MediaError::Cancelled),
                result = timeout_at(deadline, self.socket.read(&mut scratch)) => result,
            };
            match read {
                Err(_) => return Ok(false),
                Ok(Ok(0)) => {
                    return if self.ending {
                        Ok(true)
                    } else {
                        Err(MediaError::Io)
                    };
                }
                // TLS-Peers schließen teils ohne close_notify. Erst nach unserem
                // deleteStream ist das nur ein unbestätigtes Ende, kein Erfolg.
                Ok(Err(error))
                    if self.ending && error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(true);
                }
                Ok(Err(_)) => return Err(MediaError::Io),
                Ok(Ok(count)) => self.accept_read(&scratch[..count])?,
            }
            if Instant::now() >= deadline {
                // Ein bereits gelesener Fehler muss vor Fristende wirksam werden.
                while let Some(chunk) = self
                    .parser
                    .read_chunk(&mut self.input)
                    .map_err(|_| MediaError::ProtocolRejected)?
                {
                    self.handle(chunk).await?;
                }
                return Ok(false);
            }
        }
    }
    fn accept_read(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Err(MediaError::Io);
        }
        if data.len() > INPUT_BYTES.saturating_sub(self.input.len()) {
            return Err(MediaError::ResourceLimit);
        }
        self.received = self
            .received
            .checked_add(data.len() as u64)
            .ok_or(MediaError::ResourceLimit)?;
        self.input.extend_from_slice(data);
        Ok(())
    }
    async fn next_response(&mut self) -> Result<Response> {
        let mut scratch = [0_u8; 16 * 1024];
        loop {
            if let Some(chunk) = self
                .parser
                .read_chunk(&mut self.input)
                .map_err(|_| MediaError::ProtocolRejected)?
            {
                return self.handle(chunk).await;
            }
            self.ack_if_due().await?;
            let count = self
                .socket
                .read(&mut scratch)
                .await
                .map_err(|_| MediaError::Io)?;
            self.accept_read(&scratch[..count])?;
        }
    }
    async fn handle(&mut self, chunk: Chunk) -> Result<Response> {
        if self.control_epoch.elapsed() >= Duration::from_secs(1) {
            self.control_epoch = Instant::now();
            self.control_count = 0;
        }
        self.control_count += 1;
        if self.control_count > COMMAND_LIMIT {
            return Err(MediaError::ResourceLimit);
        }
        let body = &chunk.payload;
        if chunk.message_header.msg_type_id.0 <= 6
            && (chunk.message_header.msg_stream_id != 0 || chunk.basic_header.chunk_stream_id != 2)
        {
            return Err(MediaError::ProtocolRejected);
        }
        match chunk.message_header.msg_type_id.0 {
            1 => {
                let size = read_u32(body)?;
                if !self.parser.update_max_chunk_size(size as usize) {
                    return Err(MediaError::ProtocolRejected);
                }
            }
            3 => {
                let sequence = read_u32(body)?;
                let delta = sequence.wrapping_sub(self.peer_acknowledged as u32) as u64;
                if delta > self.sent.saturating_sub(self.peer_acknowledged) {
                    return Err(MediaError::ProtocolRejected);
                }
                self.peer_acknowledged += delta;
            }
            5 => {
                let window = read_u32(body)?;
                if window == 0 {
                    return Err(MediaError::ProtocolRejected);
                }
                self.window = window;
                self.ack_if_due().await?;
            }
            6 => {
                if body.len() != 5 || body[4] > 2 || read_u32(&body[..4])? == 0 {
                    return Err(MediaError::ProtocolRejected);
                }
                let requested = read_u32(&body[..4])?;
                let updated = match body[4] {
                    0 => Some((requested, 0)),
                    1 => Some((
                        self.peer_window
                            .map_or(requested, |(old, _)| old.min(requested)),
                        1,
                    )),
                    2 if self.peer_window.is_some_and(|(_, kind)| kind == 0) => {
                        Some((requested, 0))
                    }
                    _ => self.peer_window,
                };
                if updated != self.peer_window {
                    self.peer_window = updated;
                    if let Some((window, _)) = updated {
                        self.control(5, &window.to_be_bytes()).await?;
                    }
                }
            }
            4 => {
                if body.len() < 2 {
                    return Err(MediaError::ProtocolRejected);
                }
                let event = u16::from_be_bytes([body[0], body[1]]);
                match event {
                    6 => {
                        if body.len() != 6 {
                            return Err(MediaError::ProtocolRejected);
                        }
                        let mut reply = body.to_vec();
                        reply[..2].copy_from_slice(&7_u16.to_be_bytes());
                        self.control(4, &reply).await?;
                    }
                    0 | 1 | 2 | 4 | 7 => {
                        if body.len() != 6 {
                            return Err(MediaError::ProtocolRejected);
                        }
                        if event == 1
                            && read_u32(&body[2..])? == self.stream_id
                            && self.stream_id != 0
                            && !self.ending
                        {
                            return Err(MediaError::Io);
                        }
                    }
                    3 => {
                        if body.len() != 10 {
                            return Err(MediaError::ProtocolRejected);
                        }
                    }
                    _ => return Err(MediaError::ProtocolRejected),
                }
            }
            20 | 17 => {
                if chunk.message_header.msg_stream_id != 0
                    && chunk.message_header.msg_stream_id != self.stream_id
                {
                    return Err(MediaError::ProtocolRejected);
                }
                let command = if chunk.message_header.msg_type_id.0 == 17 {
                    body.strip_prefix(&[0])
                        .ok_or(MediaError::ProtocolRejected)?
                } else {
                    body.as_ref()
                };
                let values = Amf0Decoder::from_slice_with_limits(
                    command,
                    DecodeLimits {
                        max_input_bytes: CONTROL_BYTES,
                        max_string_bytes: 16 * 1024,
                        max_container_entries: 128,
                        max_total_values: 1024,
                        max_depth: 12,
                    },
                )
                .decode_all()
                .map_err(|_| MediaError::ProtocolRejected)?;
                let response = response(&values)?;
                if response == Response::Unpublished && !self.ending {
                    return Err(MediaError::PublishRejected);
                }
                return Ok(response);
            }
            // Publishing has no inbound media/aggregate/Abort semantics. Fail
            // closed instead of accumulating unsupported state or raw payloads.
            _ => return Err(MediaError::ProtocolRejected),
        }
        Ok(Response::Other)
    }
    async fn ack_if_due(&mut self) -> Result<()> {
        if self.received.saturating_sub(self.acknowledged) >= u64::from(self.window) {
            self.control(3, &(self.received as u32).to_be_bytes())
                .await?;
            self.acknowledged = self.received;
        }
        Ok(())
    }
    async fn control(&mut self, kind: u8, body: &[u8]) -> Result<()> {
        self.write(2, 0, kind, 0, Bytes::copy_from_slice(body))
            .await
    }
    async fn command(
        &mut self,
        name: &str,
        transaction: f64,
        stream_id: u32,
        arguments: &[Amf0Value<'_>],
    ) -> Result<()> {
        let mut payload = Zeroizing::new(Vec::new());
        {
            let mut encoder = Amf0Encoder::new(&mut *payload);
            encoder
                .encode_string(name)
                .map_err(|_| MediaError::ProtocolRejected)?;
            encoder
                .encode_number(transaction)
                .map_err(|_| MediaError::ProtocolRejected)?;
            for value in arguments {
                value
                    .encode(&mut encoder)
                    .map_err(|_| MediaError::ProtocolRejected)?;
            }
        }
        self.write(3, 0, 20, stream_id, Bytes::from_owner(payload))
            .await
    }
    async fn write(
        &mut self,
        csid: u32,
        timestamp: u32,
        kind: u8,
        stream_id: u32,
        body: Bytes,
    ) -> Result<()> {
        // Begrenzter Nachrichtenkörper plus feste Chunk-Header. Zeroizing hält
        // auch vorübergehend codierte publish-Kommandos aus freigegebenem RAM fern.
        if body.len() > 0xff_ffff {
            return Err(MediaError::ResourceLimit);
        }
        let body_len = body.len();
        let mut wire = Zeroizing::new(Vec::with_capacity(
            body.len().saturating_add(body.len() / 128 * 5 + 32),
        ));
        self.writer
            .write_chunk(
                &mut *wire,
                Chunk::new(csid, timestamp, MessageType(kind), stream_id, body),
            )
            .map_err(|_| MediaError::ProtocolRejected)?;
        let deadline = Instant::now() + self.write_timeout;
        if matches!(kind, 8 | 9 | 18) {
            // Am Chunk-Rand darf ein Kontrollpaket eingeschoben werden. Die
            // Bandbreitenreserve kann höchstens um einen Chunk überschritten
            // werden; niemals um einen beliebig großen Video-Keyframe.
            let mut offset = 0;
            let mut remaining = body_len;
            let extension = usize::from(timestamp >= 0xff_ffff) * 4;
            while remaining > 0 {
                while self.peer_window.is_some_and(|(window, _)| {
                    self.sent.saturating_sub(self.peer_acknowledged) >= u64::from(window)
                }) {
                    timeout_at(deadline, Box::pin(self.next_response()))
                        .await
                        .map_err(|_| MediaError::Io)??;
                }
                let payload = remaining.min(self.outgoing_chunk_size);
                let end = offset + if offset == 0 { 12 } else { 1 } + extension + payload;
                timeout_at(deadline, async {
                    self.socket.write_all(&wire[offset..end]).await?;
                    self.socket.flush().await
                })
                .await
                .map_err(|_| MediaError::Io)?
                .map_err(|_| MediaError::Io)?;
                self.sent = self
                    .sent
                    .checked_add((end - offset) as u64)
                    .ok_or(MediaError::ResourceLimit)?;
                offset = end;
                remaining -= payload;
            }
        } else {
            timeout_at(deadline, async {
                self.socket.write_all(&wire).await?;
                self.socket.flush().await
            })
            .await
            .map_err(|_| MediaError::Io)?
            .map_err(|_| MediaError::Io)?;
            self.sent = self
                .sent
                .checked_add(wire.len() as u64)
                .ok_or(MediaError::ResourceLimit)?;
        }
        Ok(())
    }
}
fn read_u32(body: &[u8]) -> Result<u32> {
    Ok(u32::from_be_bytes(
        body.try_into().map_err(|_| MediaError::ProtocolRejected)?,
    ))
}
fn text<'a>(value: &'a Amf0Value<'_>) -> Option<&'a str> {
    if let Amf0Value::String(s) = value {
        Some(s.as_str())
    } else {
        None
    }
}
fn property<'a>(value: &'a Amf0Value<'_>, name: &str) -> Option<&'a str> {
    if let Amf0Value::Object(object) = value {
        object
            .iter()
            .find_map(|(key, value)| (key.as_str() == name).then(|| text(value)).flatten())
    } else {
        None
    }
}
fn response(values: &[Amf0Value<'_>]) -> Result<Response> {
    let name = values
        .first()
        .and_then(text)
        .ok_or(MediaError::ProtocolRejected)?;
    let transaction = match values.get(1) {
        Some(Amf0Value::Number(n)) if n.is_finite() => *n,
        _ => return Err(MediaError::ProtocolRejected),
    };
    if name == "_error" {
        return Err(MediaError::PublishRejected);
    }
    let info = values.get(3);
    if info.and_then(|v| property(v, "level")) == Some("error") {
        return Err(MediaError::PublishRejected);
    }
    if name == "_result" && transaction == 1.0 {
        if info.and_then(|v| property(v, "code")) != Some("NetConnection.Connect.Success")
            || info.and_then(|v| property(v, "level")) != Some("status")
        {
            return Err(MediaError::PublishRejected);
        }
        return Ok(Response::Connected);
    }
    if name == "_result" && transaction == 2.0 {
        if let Some(Amf0Value::Number(id)) = info
            && id.is_finite()
            && id.fract() == 0.0
            && *id > 0.0
            && *id <= u32::MAX as f64
        {
            return Ok(Response::Created(*id as u32));
        }
        return Err(MediaError::ProtocolRejected);
    }
    if name == "onStatus" {
        match info.and_then(|v| property(v, "code")) {
            Some("NetStream.Publish.Start") => {
                if info.and_then(|v| property(v, "level")) != Some("status") {
                    return Err(MediaError::ProtocolRejected);
                }
                return Ok(Response::Published);
            }
            Some("NetStream.Publish.BadName" | "NetStream.Publish.Denied" | "NetStream.Failed") => {
                return Err(MediaError::PublishRejected);
            }
            Some("NetStream.Unpublish.Success") => {
                if info.and_then(|v| property(v, "level")) != Some("status") {
                    return Err(MediaError::ProtocolRejected);
                }
                return Ok(Response::Unpublished);
            }
            _ => (),
        }
    }
    Ok(Response::Other)
}
async fn handshake(socket: &mut Box<dyn Socket>) -> Result<()> {
    let mut c0c1 = [0_u8; 1537];
    c0c1[0] = 3;
    getrandom::fill(&mut c0c1[9..]).map_err(|_| MediaError::Io)?;
    socket.write_all(&c0c1).await.map_err(|_| MediaError::Io)?;
    socket.flush().await.map_err(|_| MediaError::Io)?;
    let mut server = [0_u8; 3073];
    socket
        .read_exact(&mut server)
        .await
        .map_err(|_| MediaError::Io)?;
    if server[0] != 3 || server[1537..1541] != c0c1[1..5] || server[1545..] != c0c1[9..] {
        return Err(MediaError::ProtocolRejected);
    }
    socket
        .write_all(&server[1..1537])
        .await
        .map_err(|_| MediaError::Io)?;
    socket.flush().await.map_err(|_| MediaError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;

    #[test]
    fn localhost_test_target_rejects_non_loopback_resolution() {
        for address in ["8.8.8.8", "2606:4700::1111", "10.1.1.1", "::ffff:8.8.8.8"] {
            assert!(!permitted_target_ip(
                address.parse().unwrap(),
                "localhost",
                true
            ));
        }
        for address in ["127.0.0.1", "::1"] {
            assert!(permitted_target_ip(
                address.parse().unwrap(),
                "localhost",
                true
            ));
            assert!(!permitted_target_ip(
                address.parse().unwrap(),
                "localhost",
                false
            ));
        }
        assert!(permitted_target_ip(
            "8.8.8.8".parse().unwrap(),
            "ingest.example",
            false
        ));
    }

    /// deleteStream wird vollständig geschrieben, während im selben Poll die
    /// Antwort lesbar wird. Kein Scheduling-Zufall und keine echte Gegenstelle.
    struct CompletionSocket {
        reply: Bytes,
        wrote: bool,
    }
    impl AsyncRead for CompletionSocket {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if !self.wrote || self.reply.is_empty() {
                return Poll::Pending;
            }
            let n = buf.remaining().min(self.reply.len());
            buf.put_slice(&self.reply.split_to(n));
            Poll::Ready(Ok(()))
        }
    }
    impl AsyncWrite for CompletionSocket {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.wrote = true;
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn ready_remote_error_wins_over_completed_delete_write() {
        let (mut client, _peer) = control_client();
        let mut body = Vec::new();
        let mut encoder = Amf0Encoder::new(&mut body);
        encoder.encode_string("_error").unwrap();
        encoder.encode_number(0.0).unwrap();
        let mut reply = Vec::new();
        ChunkWriter::default()
            .write_chunk(
                &mut reply,
                Chunk::new(3, 0, MessageType(20), 1, Bytes::from(body)),
            )
            .unwrap();
        client.socket = Box::new(CompletionSocket {
            reply: reply.into(),
            wrote: false,
        });
        let (sender, receiver) = queue::bounded(&MediaLimits::default()).unwrap();
        drop(sender);
        let (_cancel, stop) = watch::channel(false);
        let (updates, _status) = watch::channel(OutputStatus {
            id: "local-test".into(),
            state: OutputState::Publishing,
            received_events: 0,
            received_bytes: 0,
        });
        assert_eq!(
            client.run(receiver, stop, &updates).await,
            Err(MediaError::PublishRejected)
        );
        assert_ne!(updates.borrow().state, OutputState::LocalEndUnconfirmed);
    }
    fn control_client() -> (Client, tokio::io::DuplexStream) {
        let (socket, peer) = tokio::io::duplex(8192);
        (
            Client {
                socket: Box::new(socket),
                parser: ChunkReader::default(),
                writer: ChunkWriter::default(),
                input: BytesMut::new(),
                stream_id: 1,
                received: 0,
                acknowledged: 0,
                window: 4096,
                sent: 0,
                peer_acknowledged: 0,
                peer_window: None,
                outgoing_chunk_size: 128,
                write_timeout: Duration::from_secs(1),
                end_observation: Duration::from_millis(20),
                ending: false,
                control_epoch: Instant::now(),
                control_count: 0,
            },
            peer,
        )
    }
    #[tokio::test]
    async fn acknowledgements_wrap_without_accepting_unsent_bytes() {
        let (mut client, _peer) = control_client();
        client.peer_acknowledged = 0xffff_fff0;
        client.sent = 0x1_0000_0020;
        assert!(
            client
                .handle(Chunk::new(
                    2,
                    0,
                    MessageType(3),
                    0,
                    Bytes::copy_from_slice(&0x10_u32.to_be_bytes())
                ))
                .await
                .is_ok()
        );
        assert_eq!(client.peer_acknowledged, 0x1_0000_0010);
        assert!(
            client
                .handle(Chunk::new(
                    2,
                    0,
                    MessageType(3),
                    0,
                    Bytes::copy_from_slice(&0x21_u32.to_be_bytes())
                ))
                .await
                .is_err()
        );
        assert_eq!(client.peer_acknowledged, 0x1_0000_0010);
    }
    #[tokio::test]
    async fn bandwidth_hard_soft_and_dynamic_follow_previous_effective_limit() {
        let (mut client, _peer) = control_client();
        for (window, kind, expected) in [
            (4096_u32, 2, None),
            (10000, 0, Some((10000, 0))),
            (20000, 1, Some((10000, 1))),
            (30000, 2, Some((10000, 1))),
            (12000, 0, Some((12000, 0))),
            (20000, 2, Some((20000, 0))),
            (5000, 1, Some((5000, 1))),
        ] {
            let mut body = window.to_be_bytes().to_vec();
            body.push(kind);
            assert!(
                client
                    .handle(Chunk::new(2, 0, MessageType(6), 0, Bytes::from(body)))
                    .await
                    .is_ok()
            );
            assert_eq!(client.peer_window, expected);
        }
    }
    #[test]
    fn special_networks_are_rejected_including_ipv4_mapped_and_transition() {
        for ip in [
            "0.0.0.0",
            "10.1.1.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.1.1",
            "192.168.1.1",
            "198.18.0.1",
            "203.0.113.2",
            "224.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2002:7f00:1::",
            "2001:db8::1",
            "3fff::1",
        ] {
            assert!(!permitted_ip(ip.parse().unwrap(), false), "{ip}");
        }
        assert!(permitted_ip("8.8.8.8".parse().unwrap(), false));
        assert!(permitted_ip("2606:4700::1111".parse().unwrap(), false));
        assert!(permitted_ip("127.0.0.1".parse().unwrap(), true));
        assert!(!permitted_ip("10.1.1.1".parse().unwrap(), true));
    }
    #[test]
    fn malformed_create_stream_result_is_not_accepted() {
        for id in [
            0.0,
            -1.0,
            0.5,
            f64::INFINITY,
            f64::NAN,
            u32::MAX as f64 + 1.0,
        ] {
            assert!(
                response(&[
                    Amf0Value::String("_result".into()),
                    Amf0Value::Number(2.0),
                    Amf0Value::Null,
                    Amf0Value::Number(id)
                ])
                .is_err()
            );
        }
    }
}
