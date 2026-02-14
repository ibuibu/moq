// --- Publisher-specific MoQ Messages ---
function encodePublish(namespace, name) {
  const msgType = encodeVarInt(0x1D); // PUBLISH (draft-15)
  const ns = encodeString(namespace);
  const nm = encodeString(name);
  const payload = concatBytes([ns, nm]);
  const length = encodeUint16(payload.length);
  return concatBytes([msgType, length, payload]);
}

function encodeObjectHeader(namespace, name, groupId, objectId, payloadLength) {
  const ns = encodeString(namespace);
  const nm = encodeString(name);
  const gid = encodeVarInt(groupId);
  const oid = encodeVarInt(objectId);
  const plen = encodeVarInt(payloadLength);
  return concatBytes([ns, nm, gid, oid, plen]);
}

function decodeMessage(buf) {
  let offset = 0;
  const { value: msgType, bytesRead: b1 } = decodeVarInt(buf, offset);
  offset += b1;

  // Length framing: u16 big-endian
  if (offset + 2 > buf.length) throw new Error('not enough data for message length');
  const length = new DataView(buf.buffer, buf.byteOffset + offset).getUint16(0);
  offset += 2;
  if (offset + length > buf.length) throw new Error('not enough data for message payload');
  const payloadStart = offset;

  if (msgType === 0x21) { // SERVER_SETUP (draft-15)
    const { value: numParams, bytesRead: b2 } = decodeVarInt(buf, offset);
    offset += b2;
    for (let i = 0; i < numParams; i++) {
      const { value: key, bytesRead: kb } = decodeVarInt(buf, offset);
      offset += kb;
      const { value: valLen, bytesRead: vlb } = decodeVarInt(buf, offset);
      offset += vlb;
      offset += valLen; // skip value
    }
    return { type: 'ServerSetup', totalBytes: payloadStart + length };
  }

  if (msgType === 0x1E) { // PUBLISH_OK (draft-15)
    const { value: ns, bytesRead: b2 } = decodeString(buf, offset);
    offset += b2;
    const { value: name, bytesRead: b3 } = decodeString(buf, offset);
    offset += b3;
    return { type: 'PublishOk', namespace: ns, name, totalBytes: payloadStart + length };
  }

  throw new Error('unknown message type: 0x' + msgType.toString(16));
}

// --- Main ---
const statusEl = document.getElementById('status');
const statsEl = document.getElementById('stats');
const preview = document.getElementById('preview');
const canvas = document.getElementById('canvas');
const startBtn = document.getElementById('startBtn');
const stopBtn = document.getElementById('stopBtn');
const ctx = canvas.getContext('2d');

let transport = null;
let publishing = false;
let intervalId = null;
let encoder = null;
let groupId = -1;
let objectIdInGroup = 0;
let totalFramesSent = 0;
let frameCount = 0;
let fpsStartTime = 0;
const KEY_FRAME_INTERVAL = 30; // 15fps * 2sec

let audioEncoder = null;
let audioObjectId = 0;
let audioProcessorReader = null;

function setStatus(text, cls) {
  statusEl.textContent = text;
  statusEl.className = cls || '';
}

let useTestPattern = false;
let testPatternFrame = 0;

function drawTestPattern() {
  testPatternFrame++;
  const w = canvas.width, h = canvas.height;
  // 背景グラデーション
  const grad = ctx.createLinearGradient(0, 0, w, h);
  grad.addColorStop(0, '#1a1a2e');
  grad.addColorStop(1, '#16213e');
  ctx.fillStyle = grad;
  ctx.fillRect(0, 0, w, h);
  // カラーバー
  const colors = ['#ff0000','#00ff00','#0000ff','#ffff00','#ff00ff','#00ffff','#ffffff'];
  const barW = w / colors.length;
  for (let i = 0; i < colors.length; i++) {
    ctx.fillStyle = colors[i];
    ctx.fillRect(i * barW, h * 0.1, barW, h * 0.3);
  }
  // 動くボックス
  const boxX = (testPatternFrame * 3) % w;
  ctx.fillStyle = '#f0a';
  ctx.fillRect(boxX, h * 0.55, 60, 60);
  // テキスト
  ctx.fillStyle = '#fff';
  ctx.font = '24px monospace';
  ctx.textAlign = 'center';
  ctx.fillText('MoQ Test Pattern', w / 2, h * 0.88);
  ctx.font = '16px monospace';
  ctx.fillText('Frame: ' + testPatternFrame, w / 2, h * 0.95);
}

// カメラ取得（失敗時はテストパターンにフォールバック）
async function initCamera() {
  canvas.width = 640;
  canvas.height = 480;
  try {
    const stream = await navigator.mediaDevices.getUserMedia({
      video: { width: 640, height: 480 },
      audio: { sampleRate: 48000, channelCount: 1 }
    });
    preview.srcObject = stream;
    setStatus('Camera + Mic ready. Click "Start Publishing".', '');
  } catch (e) {
    console.warn('Camera unavailable, using test pattern:', e.message);
    useTestPattern = true;
    preview.style.display = 'none';
    canvas.style.display = 'block';
    canvas.style.maxWidth = '90vw';
    canvas.style.maxHeight = '60vh';
    canvas.style.border = '2px solid #333';
    drawTestPattern();
    setStatus('No camera - using test pattern. Click "Start Publishing".', '');
  }
  startBtn.disabled = false;
}

async function startPublishing() {
  startBtn.disabled = true;
  try {
    setStatus('Connecting to WebTransport...');
    transport = new WebTransport('https://' + HOST_IP + ':4433', {
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

    // ClientSetup 送信 → ServerSetup 受信
    await writer.write(encodeClientSetup());
    let { msg: serverSetup, remaining } = await readMessage(reader);
    if (serverSetup.type !== 'ServerSetup') {
      throw new Error('Expected ServerSetup, got ' + serverSetup.type);
    }
    console.log('ServerSetup received');

    // Publish (video) 送信 → PublishOk 受信
    await writer.write(encodePublish('live', 'video'));
    let result = await readMessage(reader, remaining);
    if (result.msg.type !== 'PublishOk') {
      throw new Error('Expected PublishOk, got ' + result.msg.type);
    }
    console.log('PublishOk received for ' + result.msg.namespace + '/' + result.msg.name);

    // Publish (audio) 送信 → PublishOk 受信 (テストパターンモードではスキップ)
    if (!useTestPattern) {
      await writer.write(encodePublish('live', 'audio'));
      let audioResult = await readMessage(reader, result.remaining);
      result.remaining = audioResult.remaining;
      if (audioResult.msg.type !== 'PublishOk') {
        throw new Error('Expected PublishOk for audio, got ' + audioResult.msg.type);
      }
      console.log('PublishOk received for ' + audioResult.msg.namespace + '/' + audioResult.msg.name);
    }

    // H.264 VideoEncoder 初期化
    encoder = new VideoEncoder({
      output: async (chunk, metadata) => {
        try {
          const buf = new Uint8Array(chunk.byteLength);
          chunk.copyTo(buf);
          const isKey = chunk.type === 'key';
          if (isKey) {
            groupId++;
            objectIdInGroup = 0;
          }
          const header = encodeObjectHeader('live', 'video', groupId, objectIdInGroup, buf.length);
          objectIdInGroup++;

          const uni = await transport.createUnidirectionalStream();
          const w = uni.getWriter();
          await w.write(concatBytes([header, buf]));
          await w.close();

          frameCount++;
          const elapsed = (performance.now() - fpsStartTime) / 1000;
          const fps = elapsed > 0 ? (frameCount / elapsed).toFixed(1) : '0';
          statsEl.textContent = `Frames: ${frameCount} | FPS: ${fps} | Size: ${buf.length} bytes | ${isKey ? 'KEY' : 'delta'}`;
          if (elapsed > 5) {
            frameCount = 0;
            fpsStartTime = performance.now();
          }
        } catch (e) {
          console.error('send error:', e);
          stopPublishing();
          setStatus('Send error: ' + e.message, 'error');
        }
      },
      error: (e) => {
        console.error('encoder error:', e);
        stopPublishing();
        setStatus('Encoder error: ' + e.message, 'error');
      }
    });
    encoder.configure({
      codec: 'avc1.42001f',
      width: 640,
      height: 480,
      bitrate: 1_000_000,
      framerate: 15,
      avc: { format: 'annexb' },
    });

    publishing = true;

    // AudioEncoder 初期化 + 音声処理ループ (テストパターンモードではスキップ)
    if (!useTestPattern) {
      audioObjectId = 0;
      audioEncoder = new AudioEncoder({
        output: async (chunk) => {
          try {
            const buf = new Uint8Array(chunk.byteLength);
            chunk.copyTo(buf);
            const header = encodeObjectHeader('live', 'audio', 0, audioObjectId, buf.length);
            audioObjectId++;

            const uni = await transport.createUnidirectionalStream();
            const w = uni.getWriter();
            await w.write(concatBytes([header, buf]));
            await w.close();
          } catch (e) {
            console.error('audio send error:', e);
          }
        },
        error: (e) => {
          console.error('audio encoder error:', e);
        }
      });
      audioEncoder.configure({
        codec: 'opus',
        sampleRate: 48000,
        numberOfChannels: 1,
        bitrate: 64000,
      });

      // MediaStreamTrackProcessor で音声トラック → AudioData → encode
      const audioTrack = preview.srcObject.getAudioTracks()[0];
      if (audioTrack) {
        const audioProcessor = new MediaStreamTrackProcessor({ track: audioTrack });
        audioProcessorReader = audioProcessor.readable.getReader();
        (async () => {
          while (publishing) {
            const { done, value: audioData } = await audioProcessorReader.read();
            if (done) break;
            if (audioEncoder && audioEncoder.state !== 'closed') {
              audioEncoder.encode(audioData);
            }
            audioData.close();
          }
        })();
      }
    }

    totalFramesSent = 0;
    groupId = -1;
    objectIdInGroup = 0;
    frameCount = 0;
    fpsStartTime = performance.now();
    stopBtn.disabled = false;
    setStatus('Publishing...', 'connected');

    // フレーム送信ループ (15 FPS)
    intervalId = setInterval(() => sendFrame(), 67);
  } catch (e) {
    console.error(e);
    setStatus('Error: ' + e.message, 'error');
    startBtn.disabled = false;
  }
}

function sendFrame() {
  if (!publishing || !transport || !encoder) return;

  if (useTestPattern) {
    drawTestPattern();
  } else {
    ctx.drawImage(preview, 0, 0, canvas.width, canvas.height);
  }

  const frame = new VideoFrame(canvas, { timestamp: totalFramesSent * Math.floor(1_000_000 / 15) });
  const keyFrame = totalFramesSent % KEY_FRAME_INTERVAL === 0;
  encoder.encode(frame, { keyFrame });
  frame.close();
  totalFramesSent++;
}

function stopPublishing() {
  publishing = false;
  if (intervalId) {
    clearInterval(intervalId);
    intervalId = null;
  }
  if (encoder && encoder.state !== 'closed') {
    encoder.close();
  }
  encoder = null;
  if (audioEncoder && audioEncoder.state !== 'closed') {
    audioEncoder.close();
  }
  audioEncoder = null;
  if (audioProcessorReader) {
    audioProcessorReader.cancel();
    audioProcessorReader = null;
  }
  if (transport) {
    transport.close();
    transport = null;
  }
  startBtn.disabled = false;
  stopBtn.disabled = true;
  setStatus('Stopped.', '');
}

startBtn.addEventListener('click', startPublishing);
stopBtn.addEventListener('click', stopPublishing);

initCamera();
