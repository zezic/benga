'use strict';

const releasesUrl = 'https://github.com/zezic/benga/releases';
const downloadLinks = document.querySelectorAll('[data-platform]');
let releaseManifest;
const mirrorUrls = new Map();

function safeDownloadUrl(raw) {
  try {
    const url = new URL(raw);
    if (url.protocol !== 'https:' || url.username || url.password) return null;
    if (url.origin === 'https://github.com' && url.pathname.startsWith('/zezic/benga/releases/download/')) return url.href;
    if (url.origin === 'https://benga.zezic.ru' && url.pathname.startsWith('/releases/')) return url.href;
    if (url.origin === 'https://benga.zezic.ru' && /^\/downloads\/v\d+\.\d+\.\d+\//.test(url.pathname)) return url.href;
  } catch { /* Keep the official releases link if metadata has an invalid URL. */ }
  return null;
}

function showDownloads() {
  for (const link of downloadLinks) {
    const key = link.dataset.platform;
    const entry = releaseManifest?.platforms?.[key];
    let url = mirrorUrls.get(key) || safeDownloadUrl(entry?.download_url || entry?.url);
    if (link.dataset.format === 'deb') {
      // The signed feed describes the portable archive; the same release also
      // publishes a Debian installer. It stays on GitHub, outside that mirror.
      const archive = safeDownloadUrl(entry?.url);
      const filename = `Benga-v${releaseManifest?.version}-${key}.tar.gz`;
      url = archive?.endsWith(`/${filename}`) ? archive.slice(0, -7) + '.deb' : null;
    }
    link.href = url || releasesUrl;
  }
}

// Use release metadata when available; the official releases page remains the
// destination until packages are published, without invented download URLs.
fetch('/downloads/latest.json', { cache: 'no-store' })
  .then(response => { if (!response.ok) throw new Error('No release metadata'); return response.json(); })
  .then(manifest => {
    if (!/^\d+\.\d+\.\d+$/.test(manifest.version) || !manifest.platforms) return;
    releaseManifest = manifest;
    showDownloads();
    // Preserve signed manifest bytes and prefer only complete mirrored archives.
    if (location.origin === 'https://benga.zezic.ru') {
      for (const [key, entry] of Object.entries(manifest.platforms)) {
        const source = safeDownloadUrl(entry.download_url || entry.url);
        if (!source) continue;
        const filename = new URL(source).pathname.split('/').pop();
        if (!/^Benga-v[\w.-]+$/.test(filename)) continue;
        const mirror = `/downloads/v${manifest.version}/${filename}`;
        fetch(mirror, { method: 'HEAD', cache: 'no-store' }).then(response => {
          if (response.ok && Number(response.headers.get('content-length')) === Number(entry.size)) {
            mirrorUrls.set(key, new URL(mirror, location.origin).href);
            showDownloads();
          }
        }).catch(() => {});
      }
    }
  }).catch(() => {});

const copyButton = document.querySelector('#copy-command');
const copyStatus = document.querySelector('#copy-status');
let copyResetTimer;
copyButton.addEventListener('click', async () => {
  const code = document.querySelector('#install-command');
  try {
    await navigator.clipboard.writeText(code.textContent);
    clearTimeout(copyResetTimer);
    copyButton.classList.add('copied');
    copyStatus.textContent = 'Install command copied.';
    copyResetTimer = setTimeout(() => { copyButton.classList.remove('copied'); copyStatus.textContent = ''; }, 1500);
  } catch {
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(code);
    selection.removeAllRanges(); selection.addRange(range);
    copyStatus.textContent = 'Select and copy the command.';
  }
});
