// The video in a <video> element, for a worker that writes its decoded frames to a track
// (`./worker.ts`, Safari) instead of drawing them on the canvas. The element sits under the
// input canvas, at the place and size `letterbox` gives in device pixels, so each monitor pixel
// lands where the canvas would draw it and input maps through it in the same way.
import { letterbox } from "./input";

/** Events that may start a video the browser did not play by itself. Capture phase, as for the
 *  sound (`./sound.ts`). */
const GESTURES = ["pointerdown", "keydown"] as const;

export interface TrackView {
  /** The canvas's new size in device pixels. */
  setBox(width: number, height: number): void;
  detach(): void;
}

/** Show `track` in `parent`, under `canvas`, which covers the same area. */
export function showTrack(
  parent: HTMLElement,
  canvas: HTMLCanvasElement,
  track: MediaStreamTrack,
): TrackView {
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.disablePictureInPicture = true;
  video.className = "pointer-events-none absolute max-w-none";
  video.srcObject = new MediaStream([track]);
  parent.prepend(video);
  let box: { w: number; h: number } | null = null;

  function place() {
    const { videoWidth: w, videoHeight: h } = video;
    if (!box || !w || !h) return;
    const { scale, offX, offY } = letterbox(box.w, box.h, w, h);
    // CSS pixels per device pixel of the canvas.
    const k = canvas.getBoundingClientRect().width / box.w;
    video.style.left = `${offX * k}px`;
    video.style.top = `${offY * k}px`;
    video.style.width = `${w * scale * k}px`;
    video.style.height = `${h * scale * k}px`;
  }

  // Safari can keep even a muted video paused until the user clicks or presses a key in the
  // page (Selkies saw this): try again then.
  const play = () => {
    if (video.paused) video.play().catch(() => {});
  };
  video.addEventListener("resize", place);
  for (const g of GESTURES) window.addEventListener(g, play, true);
  play();

  return {
    setBox(width, height) {
      box = { w: width, h: height };
      place();
    },
    detach() {
      for (const g of GESTURES) window.removeEventListener(g, play, true);
      video.srcObject = null;
      track.stop();
      video.remove();
    },
  };
}
