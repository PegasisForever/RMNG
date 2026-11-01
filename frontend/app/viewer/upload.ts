// A file dropped on a clone's screen goes to the selected clone's home.

/** Upload one dropped file into the selected clone's home; resolves to the name it got there. */
export function uploadToClone(file: File, onProgress: (fraction: number) => void): Promise<string> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest(); // fetch() reports no upload progress
    xhr.open("POST", `/api/viewer/upload?name=${encodeURIComponent(file.name)}`);
    xhr.upload.onprogress = (e) => onProgress(e.lengthComputable ? e.loaded / e.total : 0);
    xhr.onload = () =>
      xhr.status === 200
        ? resolve((JSON.parse(xhr.responseText) as { name: string }).name)
        : reject(new Error(xhr.responseText || `HTTP ${xhr.status}`));
    xhr.onerror = () => reject(new Error("network error"));
    xhr.send(file);
  });
}
