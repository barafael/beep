// The box's displays in the page (testbox/web). Frames come over one
// WebSocket as binary messages, the rest as JSON; with input on, pointer and keys go back.
'use strict';

// The token is in the fragment, which the browser never sends: out of the address bar and the
// history once read.
const token = location.hash.slice(1);
history.replaceState(null, '', location.pathname);

const state = document.getElementById('state');
const main = document.getElementById('displays');
const tabs = document.getElementById('tabs');
const actualSize = document.getElementById('actual-size');
actualSize.addEventListener('change', () => {
  main.classList.toggle('actual-size', actualSize.checked);
});
/** @type {{figure: HTMLElement, canvas: HTMLCanvasElement, context: CanvasRenderingContext2D, caption: HTMLElement, name: string}[]} */
const displays = [];
let interactive = false;

const socket = new WebSocket(`ws://${location.host}/ws`);
socket.binaryType = 'arraybuffer';
socket.addEventListener('open', () => send({ type: 'token', token }));
socket.addEventListener('close', () => {
  state.textContent = 'closed';
});
socket.addEventListener('message', (event) => {
  if (typeof event.data === 'string') {
    handle(JSON.parse(event.data));
  } else {
    draw(event.data);
  }
});

function send(message) {
  if (socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(message));
  }
}

function handle(message) {
  switch (message.type) {
    case 'hello':
      interactive = message.interactive;
      message.displays.forEach(setUp);
      // One display at a time by default: side by side, each gets a fraction of the width.
      tab('All', () => show(null));
      show(0);
      state.textContent = interactive ? 'input on' : 'watching (input off)';
      break;
    case 'resize': {
      const display = displays[message.display];
      display.canvas.width = message.width;
      display.canvas.height = message.height;
      break;
    }
    case 'state': {
      const display = displays[message.display];
      display.caption.textContent = message.state === ''
        ? display.name
        : `${display.name}: ${message.state}`;
      break;
    }
  }
}

function setUp(name, index) {
  const figure = document.createElement('figure');
  const caption = document.createElement('figcaption');
  caption.textContent = name;
  const canvas = document.createElement('canvas');
  figure.append(caption, canvas);
  main.append(figure);
  displays.push({ figure, canvas, context: canvas.getContext('2d'), caption, name });
  tab(name, () => show(index));
  if (interactive) {
    takeInput(canvas, index);
  }
}

/** A button that shows what `select` picks, pressed while it is shown. */
function tab(label, select) {
  const button = document.createElement('button');
  button.textContent = label;
  button.addEventListener('click', select);
  tabs.append(button);
}

/** Shows display `index` alone, or all of them for `null`; marks its tab. */
function show(index) {
  displays.forEach((display, shown) => {
    display.figure.hidden = index !== null && shown !== index;
  });
  const pressed = index === null ? displays.length : index;
  [...tabs.children].forEach((button, position) => {
    button.setAttribute('aria-pressed', String(position === pressed));
  });
}

// [u8 display][u16 x][u16 y][u16 width][u16 height], little-endian, then RGBA row by row.
function draw(buffer) {
  const header = new DataView(buffer, 0, 9);
  const display = displays[header.getUint8(0)];
  const x = header.getUint16(1, true);
  const y = header.getUint16(3, true);
  const width = header.getUint16(5, true);
  const height = header.getUint16(7, true);
  const pixels = new Uint8ClampedArray(buffer, 9, width * height * 4);
  display.context.putImageData(new ImageData(pixels, width, height), x, y);
}

// Where an event is in the display's pixels: the canvas is scaled to fit and centred
// (`object-fit: contain`).
function framePoint(canvas, event) {
  const box = canvas.getBoundingClientRect();
  const scale = Math.min(box.width / canvas.width, box.height / canvas.height);
  return {
    x: (event.clientX - box.left - (box.width - canvas.width * scale) / 2) / scale,
    y: (event.clientY - box.top - (box.height - canvas.height * scale) / 2) / scale,
  };
}

// The DOM's buttons (1 left, 2 right, 4 middle) as RFB's (1 left, 2 middle, 4 right).
function buttons(event) {
  return (event.buttons & 1) | ((event.buttons & 4) >> 1) | ((event.buttons & 2) << 1);
}

function takeInput(canvas, display) {
  canvas.tabIndex = 0;
  const pointer = (event) => {
    event.preventDefault();
    send({ type: 'pointer', display, ...framePoint(canvas, event), buttons: buttons(event) });
  };
  canvas.addEventListener('pointerdown', (event) => {
    canvas.focus();
    pointer(event);
  });
  canvas.addEventListener('pointerup', pointer);
  canvas.addEventListener('pointermove', pointer);
  canvas.addEventListener('contextmenu', (event) => event.preventDefault());
  canvas.addEventListener('wheel', (event) => {
    event.preventDefault();
    send({ type: 'wheel', display, ...framePoint(canvas, event), up: event.deltaY < 0 });
  });
  const key = (down) => (event) => {
    event.preventDefault();
    send({ type: 'key', display, key: event.key, down });
  };
  canvas.addEventListener('keydown', key(true));
  canvas.addEventListener('keyup', key(false));
}
