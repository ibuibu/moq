// --- Viewer-specific MoQ Messages ---
function encodeSubscribe(namespace, name) {
  const msgType = encodeVarInt(0x03); // SUBSCRIBE
  const ns = encodeString(namespace);
  const nm = encodeString(name);
  return concatBytes([msgType, ns, nm]);
}

function decodeMessage(buf) {
  let offset = 0;
  const { value: msgType, bytesRead: b1 } = decodeVarInt(buf, offset);
  offset += b1;

  if (msgType === 0x41) { // SERVER_SETUP
    const { value: version, bytesRead: b2 } = decodeVarInt(buf, offset);
    offset += b2;
    const { value: numParams, bytesRead: b3 } = decodeVarInt(buf, offset);
    offset += b3;
    for (let i = 0; i < numParams; i++) {
      const { bytesRead: kb } = decodeVarInt(buf, offset);
      offset += kb;
      const { bytesRead: vb } = decodeString(buf, offset);
      offset += vb;
    }
    return { type: 'ServerSetup', version, totalBytes: offset };
  }

  if (msgType === 0x04) { // SUBSCRIBE_OK
    const { value: ns, bytesRead: b2 } = decodeString(buf, offset);
    offset += b2;
    const { value: name, bytesRead: b3 } = decodeString(buf, offset);
    offset += b3;
    return { type: 'SubscribeOk', namespace: ns, name, totalBytes: offset };
  }

  throw new Error('unknown message type: 0x' + msgType.toString(16));
}

// --- ObjectHeader decode ---
function decodeObjectHeader(buf) {
  let offset = 0;
  const { value: ns, bytesRead: b1 } = decodeString(buf, offset);
  offset += b1;
  const { value: name, bytesRead: b2 } = decodeString(buf, offset);
  offset += b2;
  const { value: groupId, bytesRead: b3 } = decodeVarInt(buf, offset);
  offset += b3;
  const { value: objectId, bytesRead: b4 } = decodeVarInt(buf, offset);
  offset += b4;
  const { value: payloadLength, bytesRead: b5 } = decodeVarInt(buf, offset);
  offset += b5;
  return { ns, name, groupId, objectId, payloadLength, headerSize: offset };
}

// --- Main ---
const statusEl = document.getElementById('status');
const statsEl = document.getElementById('stats');

let frameCount = 0;
let fpsStartTime = performance.now();
let decoder = null;
let receivedKeyFrame = false;
const GOP_SIZE = 30;

let audioDecoder = null;
let audioContext = null;
let nextAudioTime = 0;

function setStatus(text, cls) {
  statusEl.textContent = text;
  statusEl.className = cls || '';
}

async function readStream(reader) {
  const chunks = [];
  let totalLen = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    totalLen += value.length;
  }
  const result = new Uint8Array(totalLen);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}

async function start() {
  try {
    setStatus('Connecting to WebTransport...');
    const transport = new WebTransport('https://' + HOST_IP + ':4433', {
      serverCertificateHashes: [{
        algorithm: 'sha-256',
        value: CERT_HASH.buffer,
      }],
    });
    await transport.ready;
    setStatus('Connected! Setting up...', 'connected');

    // 制御ストリーム (bidirectional)
    const bidi = await transport.createBidirectionalStream();
    const writer = bidi.writable.getWriter();
    const reader = bidi.readable.getReader();

    // ClientSetup 送信
    await writer.write(encodeClientSetup());

    // ServerSetup 受信
    let { msg: serverSetup, remaining } = await readMessage(reader);
    if (serverSetup.type !== 'ServerSetup') {
      throw new Error('Expected ServerSetup, got ' + serverSetup.type);
    }
    console.log('ServerSetup received, version=0x' + serverSetup.version.toString(16));

    // Subscribe (video) 送信 → SubscribeOk 受信
    await writer.write(encodeSubscribe('live', 'video'));
    let result = await readMessage(reader, remaining);
    if (result.msg.type !== 'SubscribeOk') {
      throw new Error('Expected SubscribeOk, got ' + result.msg.type);
    }
    console.log('SubscribeOk received for ' + result.msg.namespace + '/' + result.msg.name);

    // Subscribe (audio) 送信 → SubscribeOk 受信
    await writer.write(encodeSubscribe('live', 'audio'));
    let audioResult = await readMessage(reader, result.remaining);
    if (audioResult.msg.type !== 'SubscribeOk') {
      throw new Error('Expected SubscribeOk for audio, got ' + audioResult.msg.type);
    }
    console.log('SubscribeOk received for ' + audioResult.msg.namespace + '/' + audioResult.msg.name);
    setStatus('Receiving video + audio...', 'connected');

    // H.264 VideoDecoder 初期化
    const frameCanvas = document.getElementById('frame');
    const frameCtx = frameCanvas.getContext('2d');
    decoder = new VideoDecoder({
      output: (frame) => {
        frameCtx.drawImage(frame, 0, 0);
        frame.close();
      },
      error: (e) => {
        console.error('decoder error:', e);
        setStatus('Decoder error: ' + e.message, 'error');
      }
    });
    decoder.configure({
      codec: 'avc1.42001f',
    });
    receivedKeyFrame = false;

    // AudioDecoder + AudioContext 初期化
    audioContext = new AudioContext({ sampleRate: 48000 });
    nextAudioTime = 0;

    // autoplay policy 対策: ボタンクリックで AudioContext を resume
    const unmuteBtn = document.getElementById('unmuteBtn');
    if (audioContext.state === 'suspended') {
      unmuteBtn.style.display = 'inline-block';
      unmuteBtn.addEventListener('click', () => {
        audioContext.resume();
        unmuteBtn.style.display = 'none';
      });
    }
    // ページ上の任意のクリックでも resume
    document.addEventListener('click', () => {
      if (audioContext && audioContext.state === 'suspended') {
        audioContext.resume();
        unmuteBtn.style.display = 'none';
      }
    }, { once: true });

    audioDecoder = new AudioDecoder({
      output: (audioData) => {
        // AudioData → AudioBuffer → AudioBufferSourceNode でギャップレス再生
        const numFrames = audioData.numberOfFrames;
        const sampleRate = audioData.sampleRate;
        const buffer = audioContext.createBuffer(1, numFrames, sampleRate);
        const channelData = new Float32Array(numFrames);
        audioData.copyTo(channelData, { planeIndex: 0, format: 'f32-planar' });
        buffer.copyToChannel(channelData, 0);
        audioData.close();

        const source = audioContext.createBufferSource();
        source.buffer = buffer;
        source.connect(audioContext.destination);
        const currentTime = audioContext.currentTime;
        if (nextAudioTime < currentTime) {
          nextAudioTime = currentTime;
        }
        source.start(nextAudioTime);
        nextAudioTime += numFrames / sampleRate;
      },
      error: (e) => {
        console.error('audio decoder error:', e);
      }
    });
    audioDecoder.configure({
      codec: 'opus',
      sampleRate: 48000,
      numberOfChannels: 1,
    });

    // オブジェクト受信ループ (unidirectional streams)
    const incomingReader = transport.incomingUnidirectionalStreams.getReader();
    while (true) {
      const { done, value: stream } = await incomingReader.read();
      if (done) break;

      // 各ストリームを非同期で処理
      handleObjectStream(stream).catch(e => {
        console.warn('stream error:', e);
      });
    }

    setStatus('Stream ended', '');
  } catch (e) {
    console.error(e);
    setStatus('Error: ' + e.message, 'error');
  }
}

async function handleObjectStream(stream) {
  const data = await readStream(stream.getReader());
  const header = decodeObjectHeader(data);
  const payload = data.slice(header.headerSize, header.headerSize + header.payloadLength);

  if (header.name === 'audio') {
    // Opus 音声デコード (全フレーム key)
    const chunk = new EncodedAudioChunk({
      type: 'key',
      timestamp: header.objectId * 20_000, // 20ms per frame
      data: payload,
    });
    audioDecoder.decode(chunk);
    return;
  }

  // video トラック
  const isKey = header.objectId === 0;
  if (!receivedKeyFrame) {
    if (!isKey) {
      console.log('skipping delta frame (waiting for key frame)');
      return;
    }
    receivedKeyFrame = true;
  }

  // H.264 デコード
  const type = isKey ? 'key' : 'delta';
  const timestamp = (header.groupId * GOP_SIZE + header.objectId) * Math.floor(1_000_000 / 15);
  const chunk = new EncodedVideoChunk({
    type,
    timestamp,
    data: payload,
  });
  decoder.decode(chunk);

  // Stats 更新
  frameCount++;
  const elapsed = (performance.now() - fpsStartTime) / 1000;
  const fps = elapsed > 0 ? (frameCount / elapsed).toFixed(1) : '0';
  statsEl.textContent = `Frames: ${frameCount} | FPS: ${fps} | Size: ${payload.length} bytes | ${type}`;

  // 5秒ごとにFPSカウンタリセット
  if (elapsed > 5) {
    frameCount = 0;
    fpsStartTime = performance.now();
  }
}

start();
