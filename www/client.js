
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
        const { type, msg, level } = e.data;
        if (type === 'log') {
            log(msg, level);
        }
    };

    log('Initialized Web Worker and transferred canvas control');
};
