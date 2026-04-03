use axum::response::Html;

/// The embedded web portal — a single-page app served as HTML.
/// No build step, no npm, no webpack. Just HTML + vanilla JS + CSS.
pub fn portal_html() -> Html<&'static str> {
    Html(PORTAL_HTML)
}

pub fn download_page_html() -> Html<&'static str> {
    Html(DOWNLOAD_HTML)
}

const PORTAL_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>updown</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: #0a0a0a; color: #e0e0e0; min-height: 100vh; }
a { color: #60a5fa; text-decoration: none; }
a:hover { text-decoration: underline; }

.header { background: #111; border-bottom: 1px solid #222; padding: 16px 32px; display: flex; align-items: center; justify-content: space-between; }
.header h1 { font-size: 20px; font-weight: 700; color: #fff; }
.header h1 span { color: #3b82f6; }
.header .stats { font-size: 13px; color: #888; }

.container { max-width: 1200px; margin: 0 auto; padding: 32px; }

.tabs { display: flex; gap: 4px; margin-bottom: 24px; }
.tab { padding: 10px 20px; border-radius: 8px; background: #181818; cursor: pointer; font-size: 14px; color: #888; border: 1px solid transparent; transition: all 0.15s; }
.tab:hover { color: #fff; background: #222; }
.tab.active { color: #fff; background: #1e3a5f; border-color: #3b82f6; }

.panel { display: none; }
.panel.active { display: block; }

/* Upload zone */
.upload-zone { border: 2px dashed #333; border-radius: 16px; padding: 64px; text-align: center; cursor: pointer; transition: all 0.2s; margin-bottom: 24px; }
.upload-zone:hover, .upload-zone.dragover { border-color: #3b82f6; background: #0d1b2a; }
.upload-zone h2 { font-size: 24px; margin-bottom: 8px; color: #fff; }
.upload-zone p { color: #666; font-size: 14px; }
.upload-zone input[type=file] { display: none; }

/* Cards */
.card { background: #141414; border: 1px solid #222; border-radius: 12px; padding: 20px; margin-bottom: 12px; }
.card-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 8px; }
.card-header h3 { font-size: 16px; color: #fff; }
.card-meta { font-size: 13px; color: #666; }
.badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; }
.badge.completed { background: #064e3b; color: #34d399; }
.badge.active { background: #1e3a5f; color: #60a5fa; }
.badge.pending { background: #333; color: #888; }

/* Progress bar */
.progress { height: 4px; background: #222; border-radius: 2px; overflow: hidden; margin: 8px 0; }
.progress-fill { height: 100%; background: linear-gradient(90deg, #3b82f6, #60a5fa); transition: width 0.3s; }

/* Buttons */
.btn { padding: 10px 20px; border-radius: 8px; border: none; cursor: pointer; font-size: 14px; font-weight: 500; transition: all 0.15s; }
.btn-primary { background: #3b82f6; color: #fff; }
.btn-primary:hover { background: #2563eb; }
.btn-secondary { background: #222; color: #fff; border: 1px solid #333; }
.btn-secondary:hover { background: #333; }
.btn-sm { padding: 6px 12px; font-size: 12px; }

/* Table */
table { width: 100%; border-collapse: collapse; }
th { text-align: left; padding: 12px; color: #666; font-size: 12px; text-transform: uppercase; border-bottom: 1px solid #222; }
td { padding: 12px; border-bottom: 1px solid #1a1a1a; font-size: 14px; }

/* Modal */
.modal-overlay { display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.7); z-index: 100; align-items: center; justify-content: center; }
.modal-overlay.show { display: flex; }
.modal { background: #181818; border: 1px solid #333; border-radius: 16px; padding: 32px; max-width: 500px; width: 90%; }
.modal h2 { margin-bottom: 16px; color: #fff; }
.modal input, .modal textarea { width: 100%; padding: 10px; background: #111; border: 1px solid #333; border-radius: 8px; color: #fff; font-size: 14px; margin-bottom: 12px; }
.modal .actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 16px; }

/* Share link display */
.share-url { background: #111; border: 1px solid #333; border-radius: 8px; padding: 12px; font-family: monospace; font-size: 14px; display: flex; align-items: center; justify-content: space-between; margin: 12px 0; }
.share-url code { color: #60a5fa; }
.copy-btn { background: none; border: none; color: #3b82f6; cursor: pointer; font-size: 13px; }

.empty { text-align: center; padding: 48px; color: #555; }
</style>
</head>
<body>
<div class="header">
    <h1><span>up</span>down</h1>
    <div class="stats" id="header-stats">Loading...</div>
</div>

<div class="container">
    <div class="tabs">
        <div class="tab active" onclick="showTab('upload')">Upload</div>
        <div class="tab" onclick="showTab('packages')">Packages</div>
        <div class="tab" onclick="showTab('transfers')">Transfers</div>
        <div class="tab" onclick="showTab('dropbox')">Drop Box</div>
    </div>

    <!-- Upload Panel -->
    <div class="panel active" id="panel-upload">
        <div class="upload-zone" id="upload-zone" onclick="document.getElementById('file-input').click()">
            <h2>Drop files here</h2>
            <p>or click to browse &mdash; files transfer at wire speed via UDP</p>
            <input type="file" id="file-input" multiple>
        </div>
        <div id="upload-progress"></div>
    </div>

    <!-- Packages Panel -->
    <div class="panel" id="panel-packages">
        <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:16px">
            <h2 style="color:#fff">Packages</h2>
            <button class="btn btn-primary btn-sm" onclick="showCreatePackage()">New Package</button>
        </div>
        <div id="packages-list"><div class="empty">No packages yet</div></div>
    </div>

    <!-- Transfers Panel -->
    <div class="panel" id="panel-transfers">
        <h2 style="color:#fff;margin-bottom:16px">Transfer History</h2>
        <table>
            <thead><tr><th>File</th><th>Size</th><th>Speed</th><th>Time</th><th>Status</th><th>Date</th></tr></thead>
            <tbody id="transfers-body"><tr><td colspan="6" class="empty">No transfers yet</td></tr></tbody>
        </table>
    </div>

    <!-- Drop Box Panel -->
    <div class="panel" id="panel-dropbox">
        <div class="card">
            <h2 style="color:#fff;margin-bottom:8px">Public Drop Box</h2>
            <p style="color:#888;margin-bottom:16px">Share this link to let anyone send you files at maximum speed.</p>
            <div class="share-url">
                <code id="dropbox-url">Loading...</code>
                <button class="copy-btn" onclick="copyDropboxUrl()">Copy</button>
            </div>
            <p style="color:#555;font-size:12px">Files uploaded via drop box appear in your Packages tab.</p>
        </div>
    </div>
</div>

<!-- Create Share Modal -->
<div class="modal-overlay" id="share-modal">
    <div class="modal">
        <h2>Create Share Link</h2>
        <input type="number" id="share-max" placeholder="Max downloads (leave empty for unlimited)">
        <input type="number" id="share-hours" placeholder="Expires in hours (leave empty for never)">
        <div class="actions">
            <button class="btn btn-secondary" onclick="closeModal('share-modal')">Cancel</button>
            <button class="btn btn-primary" onclick="createShare()">Create Link</button>
        </div>
        <div id="share-result"></div>
    </div>
</div>

<script>
const API = '';
let currentSharePkg = '';

function showTab(name) {
    document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'));
    document.querySelectorAll('.panel').forEach(p => p.classList.remove('active'));
    event.target.classList.add('active');
    document.getElementById('panel-' + name).classList.add('active');
    if (name === 'packages') loadPackages();
    if (name === 'transfers') loadTransfers();
}

// Upload
const zone = document.getElementById('upload-zone');
const fileInput = document.getElementById('file-input');

zone.addEventListener('dragover', e => { e.preventDefault(); zone.classList.add('dragover'); });
zone.addEventListener('dragleave', () => zone.classList.remove('dragover'));
zone.addEventListener('drop', e => {
    e.preventDefault(); zone.classList.remove('dragover');
    uploadFiles(e.dataTransfer.files);
});
fileInput.addEventListener('change', () => uploadFiles(fileInput.files));

async function uploadFiles(files) {
    const progress = document.getElementById('upload-progress');
    const formData = new FormData();
    let names = [];
    for (const f of files) { formData.append('files', f); names.push(f.name); }

    progress.innerHTML = '<div class="card"><h3 style="color:#fff">Uploading ' + names.join(', ') + '...</h3><div class="progress"><div class="progress-fill" style="width:50%"></div></div></div>';

    try {
        const resp = await fetch(API + '/api/upload', { method: 'POST', body: formData });
        const data = await resp.json();
        if (data.ok) {
            progress.innerHTML = '<div class="card"><div class="card-header"><h3 style="color:#fff">Uploaded ' + data.data.files.length + ' file(s)</h3><span class="badge completed">Done</span></div><p class="card-meta">Package: ' + data.data.package_id.slice(0,8) + ' &mdash; ' + formatBytes(data.data.total_size) + '</p><button class="btn btn-primary btn-sm" style="margin-top:8px" onclick="showShareModal(\'' + data.data.package_id + '\')">Create Share Link</button></div>';
        }
    } catch(e) { progress.innerHTML = '<div class="card" style="border-color:#dc2626"><p style="color:#ef4444">Upload failed: ' + e + '</p></div>'; }
}

// Packages
async function loadPackages() {
    const resp = await fetch(API + '/api/packages');
    const data = await resp.json();
    const list = document.getElementById('packages-list');
    if (!data.data.length) { list.innerHTML = '<div class="empty">No packages yet</div>'; return; }
    list.innerHTML = data.data.map(p => `
        <div class="card">
            <div class="card-header">
                <h3>${p.name || p.id.slice(0,8)}</h3>
                <span class="card-meta">${formatBytes(p.total_size)}</span>
            </div>
            <p class="card-meta">${p.files.length} file(s) &mdash; ${p.created_at.slice(0,10)}</p>
            <button class="btn btn-sm btn-secondary" style="margin-top:8px" onclick="showShareModal('${p.id}')">Share</button>
        </div>
    `).join('');
}

// Transfers
async function loadTransfers() {
    const resp = await fetch(API + '/api/transfers');
    const data = await resp.json();
    const body = document.getElementById('transfers-body');
    if (!data.data.length) { body.innerHTML = '<tr><td colspan="6" class="empty">No transfers yet</td></tr>'; return; }
    body.innerHTML = data.data.map(t => `
        <tr>
            <td>${t.filename}</td>
            <td>${formatBytes(t.file_size)}</td>
            <td>${t.rate_mbps > 0 ? t.rate_mbps.toFixed(0) + ' Mbps' : '-'}</td>
            <td>${t.duration_ms > 0 ? (t.duration_ms/1000).toFixed(1) + 's' : '-'}</td>
            <td><span class="badge ${t.status}">${t.status}</span></td>
            <td class="card-meta">${t.created_at.slice(0,16).replace('T',' ')}</td>
        </tr>
    `).join('');
}

// Share
function showShareModal(pkgId) {
    currentSharePkg = pkgId;
    document.getElementById('share-result').innerHTML = '';
    document.getElementById('share-modal').classList.add('show');
}
function closeModal(id) { document.getElementById(id).classList.remove('show'); }

async function createShare() {
    const max = document.getElementById('share-max').value || null;
    const hours = document.getElementById('share-hours').value || null;
    const resp = await fetch(API + '/api/share', {
        method: 'POST',
        headers: {'Content-Type':'application/json'},
        body: JSON.stringify({ package_id: currentSharePkg, max_downloads: max ? parseInt(max) : null, expires_hours: hours ? parseInt(hours) : null })
    });
    const data = await resp.json();
    if (data.ok) {
        document.getElementById('share-result').innerHTML = `<div class="share-url" style="margin-top:16px"><code>${data.data.url}</code><button class="copy-btn" onclick="navigator.clipboard.writeText('${data.data.url}')">Copy</button></div>`;
    }
}

function copyDropboxUrl() { navigator.clipboard.writeText(document.getElementById('dropbox-url').textContent); }

function formatBytes(b) {
    if (b >= 1073741824) return (b/1073741824).toFixed(1) + ' GB';
    if (b >= 1048576) return (b/1048576).toFixed(1) + ' MB';
    if (b >= 1024) return (b/1024).toFixed(1) + ' KB';
    return b + ' B';
}

// Init
fetch(API + '/api/health').then(r=>r.json()).then(d => {
    document.getElementById('header-stats').textContent = d.data.transfers_completed + ' transfers completed';
    document.getElementById('dropbox-url').textContent = location.origin + '/dropbox';
});
</script>
</body>
</html>"##;

const DOWNLOAD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>updown - Download</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: #0a0a0a; color: #e0e0e0; min-height: 100vh; display: flex; align-items: center; justify-content: center; }
.download-card { background: #141414; border: 1px solid #222; border-radius: 16px; padding: 48px; max-width: 500px; width: 90%; text-align: center; }
h1 { font-size: 28px; margin-bottom: 8px; }
h1 span { color: #3b82f6; }
.file-info { margin: 24px 0; }
.file-name { font-size: 20px; color: #fff; margin-bottom: 4px; }
.file-meta { color: #666; font-size: 14px; }
.btn { display: inline-block; padding: 14px 32px; border-radius: 8px; border: none; cursor: pointer; font-size: 16px; font-weight: 600; background: #3b82f6; color: #fff; transition: all 0.15s; }
.btn:hover { background: #2563eb; }
.note { margin-top: 16px; color: #555; font-size: 12px; }
</style>
</head>
<body>
<div class="download-card">
    <h1><span>up</span>down</h1>
    <div class="file-info">
        <div class="file-name" id="dl-filename">Loading...</div>
        <div class="file-meta" id="dl-meta"></div>
    </div>
    <button class="btn" id="dl-btn" onclick="startDownload()">Download via updown</button>
    <p class="note">Requires the updown desktop agent. <a href="/" style="color:#3b82f6">Install it</a></p>
</div>
<script>
const code = location.pathname.split('/').pop();
fetch('/api/share/' + code).then(r=>r.json()).then(data => {
    if (data.ok) {
        document.getElementById('dl-filename').textContent = data.data.package_id.slice(0,8);
        document.getElementById('dl-meta').textContent = 'Downloads: ' + data.data.download_count + (data.data.max_downloads ? '/' + data.data.max_downloads : '');
    } else {
        document.getElementById('dl-filename').textContent = 'Link not found or expired';
        document.getElementById('dl-btn').style.display = 'none';
    }
});
function startDownload() {
    // Trigger the desktop agent via updown:// protocol
    window.location = 'updown://download?code=' + code + '&server=' + location.host;
}
</script>
</body>
</html>"##;
