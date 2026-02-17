// --- Config fetch ---
async function fetchConfig() {
  try {
    const port = new URLSearchParams(location.search).get('port');
    const configUrl = port ? `http://localhost:${port}/config` : '/config';
    const res = await fetch(configUrl);
    const config = await res.json();
    console.log('Config URL:', configUrl);
    CERT_HASH = new Uint8Array(config.certHash);
    HOST_IP = config.hostIp;
    QUIC_PORT = config.port || 4433;
    console.log('Config loaded: host=' + HOST_IP + ' quic_port=' + QUIC_PORT);
  } catch (e) {
    console.warn('Failed to fetch /config, using fallback:', e.message);
    HOST_IP = location.hostname || 'localhost';
    const hashHex = prompt('Enter cert hash (hex, colon-separated):', '');
    if (hashHex) {
      CERT_HASH = new Uint8Array(hashHex.split(':').map(h => parseInt(h, 16)));
    } else {
      CERT_HASH = new Uint8Array(32);
    }
  }
}

// --- VarInt (RFC 9000 Section 16) ---
function encodeVarInt(value) {
  if (value < 0x40) {
    return new Uint8Array([value]);
  } else if (value < 0x4000) {
    const buf = new Uint8Array(2);
    new DataView(buf.buffer).setUint16(0, 0x4000 | value);
    return buf;
  } else if (value < 0x40000000) {
    const buf = new Uint8Array(4);
    new DataView(buf.buffer).setUint32(0, 0x80000000 | value);
    return buf;
  } else {
    const buf = new Uint8Array(8);
    const dv = new DataView(buf.buffer);
    // value を上位32bit / 下位32bit に分割
    const hi = Math.floor(value / 0x100000000);
    const lo = value >>> 0;
    dv.setUint32(0, 0xc0000000 | hi);
    dv.setUint32(4, lo);
    return buf;
  }
}

function decodeVarInt(buf, offset) {
  const first = buf[offset];
  const prefix = first >> 6;
  const len = 1 << prefix;
  if (offset + len > buf.length) throw new Error('not enough data for varint');

  let val = first & 0x3f;
  for (let i = 1; i < len; i++) {
    val = val * 256 + buf[offset + i];
  }
  return { value: val, bytesRead: len };
}

// --- String encode/decode ---
function encodeString(str) {
  const encoded = new TextEncoder().encode(str);
  const lenBytes = encodeVarInt(encoded.length);
  const result = new Uint8Array(lenBytes.length + encoded.length);
  result.set(lenBytes);
  result.set(encoded, lenBytes.length);
  return result;
}

function decodeString(buf, offset) {
  const { value: len, bytesRead } = decodeVarInt(buf, offset);
  offset += bytesRead;
  if (offset + len > buf.length) throw new Error('not enough data for string');
  const str = new TextDecoder().decode(buf.slice(offset, offset + len));
  return { value: str, bytesRead: bytesRead + len };
}

// --- Tuple encode/decode (draft-15 track_namespace) ---
function encodeTuple(elements) {
  const parts = [encodeVarInt(elements.length)];
  for (const el of elements) {
    parts.push(encodeString(el));
  }
  return concatBytes(parts);
}

function decodeTuple(buf, offset) {
  const { value: numElements, bytesRead: b1 } = decodeVarInt(buf, offset);
  let totalRead = b1;
  offset += b1;
  const elements = [];
  for (let i = 0; i < numElements; i++) {
    const { value: el, bytesRead: elb } = decodeString(buf, offset);
    elements.push(el);
    totalRead += elb;
    offset += elb;
  }
  return { value: elements, bytesRead: totalRead };
}

function concatBytes(arrays) {
  const totalLen = arrays.reduce((sum, a) => sum + a.length, 0);
  const result = new Uint8Array(totalLen);
  let offset = 0;
  for (const a of arrays) {
    result.set(a, offset);
    offset += a.length;
  }
  return result;
}

// --- Uint16 encode ---
function encodeUint16(value) {
  const buf = new Uint8Array(2);
  new DataView(buf.buffer).setUint16(0, value);
  return buf;
}

// --- MoQ Messages ---
function encodeClientSetup() {
  const msgType = encodeVarInt(0x20); // CLIENT_SETUP (draft-15)
  // Payload: num_params(varint=0)
  const payload = encodeVarInt(0);
  const length = encodeUint16(payload.length);
  return concatBytes([msgType, length, payload]);
}

// --- Base message decoder (共通メッセージ: ServerSetup / REQUEST_ERROR / GOAWAY) ---
function decodeMessageBase(buf) {
  let offset = 0;
  const { value: msgType, bytesRead: b1 } = decodeVarInt(buf, offset);
  offset += b1;

  // Length framing: u16 big-endian
  if (offset + 2 > buf.length) throw new Error('not enough data for message length');
  const length = new DataView(buf.buffer, buf.byteOffset + offset).getUint16(0);
  offset += 2;
  if (offset + length > buf.length) throw new Error('not enough data for message payload');
  const payloadStart = offset;
  const totalBytes = payloadStart + length;

  if (msgType === 0x21) { // SERVER_SETUP (draft-15)
    const { value: numParams, bytesRead: b2 } = decodeVarInt(buf, offset);
    offset += b2;
    for (let i = 0; i < numParams; i++) {
      const { value: key, bytesRead: kb } = decodeVarInt(buf, offset);
      offset += kb;
      const { value: valLen, bytesRead: vlb } = decodeVarInt(buf, offset);
      offset += vlb;
      offset += valLen;
    }
    return { type: 'ServerSetup', totalBytes };
  }

  if (msgType === 0x05) { // REQUEST_ERROR
    const { value: subscribeId, bytesRead: b2 } = decodeVarInt(buf, offset);
    offset += b2;
    const { value: errorCode, bytesRead: b3 } = decodeVarInt(buf, offset);
    offset += b3;
    const { value: reasonPhrase, bytesRead: b4 } = decodeString(buf, offset);
    offset += b4;
    return { type: 'RequestError', subscribeId, errorCode, reasonPhrase, totalBytes };
  }

  if (msgType === 0x10) { // GOAWAY
    const { value: newSessionUri, bytesRead: b2 } = decodeString(buf, offset);
    offset += b2;
    return { type: 'GoAway', newSessionUri, totalBytes };
  }

  return { type: null, msgType, offset: payloadStart, length, totalBytes };
}


async function readMessage(reader, existingBuf) {
  let buf = existingBuf || new Uint8Array(0);

  while (true) {
    try {
      const msg = decodeMessage(buf);
      const remaining = buf.slice(msg.totalBytes);
      return { msg, remaining };
    } catch (e) {
      // need more data
    }
    const { done, value } = await reader.read();
    if (done) throw new Error('stream closed while reading message');
    const newBuf = new Uint8Array(buf.length + value.length);
    newBuf.set(buf);
    newBuf.set(value, buf.length);
    buf = newBuf;
  }
}
