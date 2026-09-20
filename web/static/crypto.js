(() => {
  const encoder = new TextEncoder();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  const context = encoder.encode('tcomp-e2ee-v1');
  const headerLength = 30;
  const maxFrame = 8 * 1024 * 1024;

  function concat(...parts) {
    const result = new Uint8Array(parts.reduce((size, part) => size + part.length, 0));
    let offset = 0;
    for (const part of parts) { result.set(part, offset); offset += part.length; }
    return result;
  }

  function encode(bytes) {
    return btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
  }

  function decode(value) {
    if (!/^[A-Za-z0-9_-]{43}$/.test(value)) throw new Error('Invalid encryption key. Open the complete watch link.');
    const bytes = Uint8Array.from(atob(value.replaceAll('-', '+').replaceAll('_', '/') + '='), c => c.charCodeAt(0));
    if (bytes.length !== 32 || encode(bytes) !== value) throw new Error('Invalid encryption key.');
    return bytes;
  }

  function remember(id) {
    const params = new URLSearchParams(location.hash.slice(1));
    const key = params.get('key');
    if (!key) return null;
    decode(key);
    sessionStorage.setItem(`tcomp.key.${id}`, key);
    return key;
  }

  function load(id) {
    const key = new URLSearchParams(location.hash.slice(1)).get('key');
    if (key !== null) {
      decode(key);
      try { sessionStorage.setItem(`tcomp.key.${id}`, key); } catch (_) {}
      return key;
    }
    try { return sessionStorage.getItem(`tcomp.key.${id}`); } catch (_) { return null; }
  }

  async function create(encoded, session) {
    if (!globalThis.crypto?.subtle) throw new Error('Encrypted sessions require HTTPS (or localhost).');
    const master = await crypto.subtle.importKey('raw', decode(encoded), 'HKDF', false, ['deriveKey']);
    const sessionBytes = encoder.encode(session);
    async function derive(writer, input) {
      return crypto.subtle.deriveKey({
        name: 'HKDF', hash: 'SHA-256', salt: sessionBytes,
        info: concat(context, encoder.encode(input ? 'input' : 'output'), writer),
      }, master, { name: 'AES-GCM', length: 256 }, false, [input ? 'encrypt' : 'decrypt']);
    }
    function nonce(sequence) {
      const result = new Uint8Array(12);
      new DataView(result.buffer).setBigUint64(4, sequence);
      return result;
    }
    const writer = crypto.getRandomValues(new Uint8Array(16));
    const inputKey = await derive(writer, true);
    let sendSequence = 0n;
    let outputWriter = null;
    let outputKey = null;
    let lastSequence = null;
    let epoch = null;
    return {
      async open(data) {
        const frame = new Uint8Array(data);
        if (frame.length < headerLength + 16 || frame.length > maxFrame
            || frame[0] !== 84 || frame[1] !== 67 || frame[2] !== 77 || frame[3] !== 80
            || frame[4] !== 1 || ![1, 2].includes(frame[5])) throw new Error('Invalid encrypted frame.');
        const frameWriter = frame.slice(6, 22);
        if (outputWriter && !outputWriter.every((byte, i) => byte === frameWriter[i])) throw new Error('Encrypted stream identity changed.');
        const key = outputKey || await derive(frameWriter, false);
        const sequence = new DataView(frame.buffer, frame.byteOffset).getBigUint64(22);
        let plain;
        try {
          plain = await crypto.subtle.decrypt({
            name: 'AES-GCM', iv: nonce(sequence), tagLength: 128,
            additionalData: concat(context, sessionBytes, frame.slice(0, headerLength)),
          }, key, frame.slice(headerLength));
        } catch (_) { throw new Error('Cannot decrypt this session: wrong key or damaged data.'); }
        if (lastSequence !== null && sequence <= lastSequence) return null;
        const payload = JSON.parse(decoder.decode(plain));
        if (frame[5] === 1) {
          if (payload.t !== 'snapshot' || typeof payload.screen !== 'string'
              || !Number.isInteger(payload.cols) || payload.cols < 1 || payload.cols > 1000
              || !Number.isInteger(payload.rows) || payload.rows < 1 || payload.rows > 500
              || typeof payload.name !== 'string' || typeof payload.cmd !== 'string'
              || ![null, 'string'].includes(payload.title === null ? null : typeof payload.title)
              || ![null, 'string'].includes(payload.cwd === null ? null : typeof payload.cwd)
              || typeof payload.input !== 'boolean' || typeof payload.epoch !== 'string'
              || (payload.exit !== null && !Number.isInteger(payload.exit))) throw new Error('Invalid encrypted snapshot.');
          decode(payload.epoch);
          validateBytes(payload.carry);
          epoch = payload.epoch;
        } else {
          if (payload.t !== 'output' || lastSequence === null || sequence !== lastSequence + 1n) throw new Error('Encrypted output is incomplete. Reopen the session.');
          validateBytes(payload.bytes);
        }
        outputWriter = frameWriter;
        outputKey = key;
        lastSequence = sequence;
        return payload;
      },
      async input(bytes) {
        if (!epoch) throw new Error('Waiting for an authenticated session snapshot.');
        if (sendSequence >= 0xffffffffffffffffn) throw new Error('Encryption counter exhausted.');
        const sequence = sendSequence++;
        const header = new Uint8Array(headerLength);
        header.set([84, 67, 77, 80, 1, 3]);
        header.set(writer, 6);
        new DataView(header.buffer).setBigUint64(22, sequence);
        const plain = encoder.encode(JSON.stringify({ t: 'input', epoch, bytes: Array.from(bytes) }));
        if (plain.length > maxFrame - headerLength - 16) throw new Error('Input is too large.');
        const encrypted = await crypto.subtle.encrypt({
          name: 'AES-GCM', iv: nonce(sequence), tagLength: 128,
          additionalData: concat(context, sessionBytes, header),
        }, inputKey, plain);
        return concat(header, new Uint8Array(encrypted));
      },
    };
  }

  function validateBytes(bytes) {
    if (!Array.isArray(bytes) || bytes.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) throw new Error('Invalid encrypted terminal data.');
  }

  globalThis.TcompCrypto = { create, remember, load, encode };
})();
