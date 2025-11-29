
const LOG_LEVEL = 'info';

function log(msg, level = 'info') {
    const logDiv = document.getElementById('log');
    const entry = document.createElement('div');
    entry.textContent = `[${new Date().toLocaleTimeString()}] ${msg}`;
    if (level === 'error') entry.style.color = 'red';
    logDiv.prepend(entry);
    console.log(msg);
}

document.getElementById('connect').onclick = () => {
    const url = document.getElementById('url').value;
    const rtspUrl = document.getElementById('rtspUrl').value;
    const canvas = document.getElementById('canvas');

    // Create worker
    const worker = new Worker('worker.js');

    // Transfer canvas control to worker
    const offscreen = canvas.transferControlToOffscreen();

    worker.postMessage({
        type: 'init',
        url: url,
        rtspUrl: rtspUrl,
        canvas: offscreen
    }, [offscreen]);

    worker.onmessage = (e) => {
        const { type, msg, level, data } = e.data;
        if (type === 'log') {
            log(msg, level);
        } else if (type === 'download') {
            const blob = new Blob([data], { type: 'video/h264' });
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = `stream-${Date.now()}.h264`;
            a.click();
            URL.revokeObjectURL(url);
            log('Downloaded recorded stream');
        }
    };

    document.getElementById('startRecord').onclick = () => {
        worker.postMessage({ type: 'startRecording' });
        document.getElementById('startRecord').disabled = true;
        document.getElementById('stopRecord').disabled = false;
        log('Started recording raw stream...');
    };

    document.getElementById('stopRecord').onclick = () => {
        worker.postMessage({ type: 'stopRecording' });
        document.getElementById('startRecord').disabled = false;
        document.getElementById('stopRecord').disabled = true;
        log('Stopped recording. Preparing download...');
    };

    log('Initialized Web Worker and transferred canvas control');
};
