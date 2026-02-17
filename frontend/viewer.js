// --- Viewer-specific MoQ Messages ---
let nextSubscribeId = 0;
function encodeSubscribe(namespace, name) {
  const subscribeId = nextSubscribeId;
  const trackAlias = nextSubscribeId;
  nextSubscribeId++;
  const msgType = encodeVarInt(0x03); // SUBSCRIBE
  const sid = encodeVarInt(subscribeId);
  const alias = encodeVarInt(trackAlias);
  const ns = encodeTuple([namespace]);
  const nm = encodeString(name);
  const priority = new Uint8Array([0]); // subscriber_priority
  const groupOrder = new Uint8Array([0x01]); // GROUP_ORDER_ASCENDING
  const filterType = encodeVarInt(0x01); // FILTER_NEXT_GROUP
  const payload = concatBytes([sid, alias, ns, nm, priority, groupOrder, filterType]);
  const length = encodeUint16(payload.length);
  return concatBytes([msgType, length, payload]);
}

function decodeMessage(buf) {
  const base = decodeMessageBase(buf);
  if (base.type) return base;

  // viewer 固有: SUBSCRIBE_OK (0x04)
  let offset = base.offset;
  if (base.msgType === 0x04) {
    const { value: subscribeId, bytesRead: b2 } = decodeVarInt(buf, offset);
    offset += b2;
    const { value: expires, bytesRead: b3 } = decodeVarInt(buf, offset);
    offset += b3;
    const groupOrder = buf[offset]; offset += 1;
    const contentExists = buf[offset]; offset += 1;
    return { type: 'SubscribeOk', subscribeId, expires, groupOrder, contentExists, totalBytes: base.totalBytes };
  }

  throw new Error('unknown message type: 0x' + base.msgType.toString(16));
}

// --- SubgroupHeader + Object entry decode ---
function decodeSubgroupStream(buf) {
  let offset = 0;
  const { value: subscribeId, bytesRead: b1 } = decodeVarInt(buf, offset);
  offset += b1;
  const { value: trackAlias, bytesRead: b2 } = decodeVarInt(buf, offset);
  offset += b2;
  const { value: groupId, bytesRead: b3 } = decodeVarInt(buf, offset);
  offset += b3;
  const { value: subgroupId, bytesRead: b4 } = decodeVarInt(buf, offset);
  offset += b4;
  const publisherPriority = buf[offset]; offset += 1;
  // Object entry
  const { value: objectId, bytesRead: b5 } = decodeVarInt(buf, offset);
  offset += b5;
  const { value: payloadLength, bytesRead: b6 } = decodeVarInt(buf, offset);
  offset += b6;
  return { subscribeId, trackAlias, groupId, subgroupId, publisherPriority, objectId, payloadLength, headerSize: offset };
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

async function setupConnection() {
  setStatus('Connecting to WebTransport...');
  const transport = new WebTransport('https://' + HOST_IP + ':' + QUIC_PORT, {
    serverCertificateHashes: [{
      algorithm: 'sha-256',
      value: CERT_HASH.buffer,
    }],
  });
  await transport.ready;
  setStatus('Connected! Setting up...', 'connected');

  const bidi = await transport.createBidirectionalStream();
  const writer = bidi.writable.getWriter();
  const reader = bidi.readable.getReader();

  await writer.write(encodeClientSetup());
  let { msg: serverSetup, remaining } = await readMessage(reader);
  if (serverSetup.type !== 'ServerSetup') {
    throw new Error('Expected ServerSetup, got ' + serverSetup.type);
  }
  console.log('ServerSetup received');

  // Subscribe (video)
  await writer.write(encodeSubscribe('live', 'video'));
  let result = await readMessage(reader, remaining);
  if (result.msg.type !== 'SubscribeOk') {
    throw new Error('Expected SubscribeOk, got ' + result.msg.type);
  }
  console.log('SubscribeOk received subscribe_id=' + result.msg.subscribeId);

  // Subscribe (audio)
  await writer.write(encodeSubscribe('live', 'audio'));
  let audioResult = await readMessage(reader, result.remaining);
  if (audioResult.msg.type !== 'SubscribeOk') {
    throw new Error('Expected SubscribeOk for audio, got ' + audioResult.msg.type);
  }
  console.log('SubscribeOk received subscribe_id=' + audioResult.msg.subscribeId);
  setStatus('Receiving video + audio...', 'connected');

  return transport;
}

function initVideoDecoder(frameCanvas) {
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
}

function initAudioDecoder() {
  audioContext = new AudioContext({ sampleRate: 48000 });
  nextAudioTime = 0;

  const unmuteBtn = document.getElementById('unmuteBtn');
  if (audioContext.state === 'suspended') {
    unmuteBtn.style.display = 'inline-block';
    unmuteBtn.addEventListener('click', () => {
      audioContext.resume();
      unmuteBtn.style.display = 'none';
    });
  }
  document.addEventListener('click', () => {
    if (audioContext && audioContext.state === 'suspended') {
      audioContext.resume();
      unmuteBtn.style.display = 'none';
    }
  }, { once: true });

  audioDecoder = new AudioDecoder({
    output: (audioData) => {
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
}

async function start() {
  try {
    await fetchConfig();
    const transport = await setupConnection();
    initVideoDecoder(document.getElementById('frame'));
    initAudioDecoder();

    // オブジェクト受信ループ (unidirectional streams)
    const incomingReader = transport.incomingUnidirectionalStreams.getReader();
    while (true) {
      const { done, value: stream } = await incomingReader.read();
      if (done) break;
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

// trackAlias → trackName マッピング (subscribe 送信順)
// video=0, audio=1
const trackAliasMap = { 0: 'video', 1: 'audio' };

async function handleObjectStream(stream) {
  const data = await readStream(stream.getReader());
  const header = decodeSubgroupStream(data);
  const payload = data.slice(header.headerSize, header.headerSize + header.payloadLength);

  const trackName = trackAliasMap[header.trackAlias] || 'unknown';
  if (trackName === 'audio') {
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
