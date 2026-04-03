use axum::response::Html;

/// Main portal — the dashboard / landing page.
/// Routes:
///   /          → Dashboard (recent activity, quick actions)
///   /send      → Send files to recipients
///   /inbox     → Received packages
///   /sent      → Sent packages with delivery status
///   /submit/:id → Public upload portal (drop box)
///   /d/:code   → Share link download page
///   /admin     → Admin panel
///   /login     → Login page
pub fn portal_html() -> Html<&'static str> {
    Html(APP_HTML)
}

pub fn login_page_html() -> Html<&'static str> {
    Html(LOGIN_HTML)
}

pub fn download_page_html() -> Html<&'static str> {
    Html(DOWNLOAD_HTML)
}

pub fn submit_page_html() -> Html<&'static str> {
    Html(SUBMIT_HTML)
}

const APP_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>updown</title>
<script src="https://unpkg.com/lucide@latest"></script>
<style>
:root { --bg: #09090b; --surface: #131316; --surface2: #1a1a1f; --border: #27272a; --text: #e4e4e7; --text2: #a1a1aa; --text3: #71717a; --blue: #3b82f6; --blue2: #2563eb; --green: #22c55e; --red: #ef4444; --radius: 10px; }
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Inter, sans-serif; background: var(--bg); color: var(--text); min-height: 100vh; display: flex; }
a { color: var(--blue); text-decoration: none; }

/* Sidebar */
.sidebar { width: 240px; background: var(--surface); border-right: 1px solid var(--border); padding: 20px 0; display: flex; flex-direction: column; position: fixed; height: 100vh; }
.sidebar-logo { padding: 0 20px 24px; font-size: 22px; font-weight: 700; }
.sidebar-logo span { color: var(--blue); }
.sidebar-nav { flex: 1; }
.nav-section { padding: 0 12px; margin-bottom: 20px; }
.nav-section-label { font-size: 11px; text-transform: uppercase; color: var(--text3); padding: 0 8px; margin-bottom: 6px; letter-spacing: 0.5px; }
.nav-item { display: flex; align-items: center; gap: 10px; padding: 9px 12px; border-radius: 8px; color: var(--text2); font-size: 14px; cursor: pointer; transition: all 0.1s; }
.nav-item:hover { background: var(--surface2); color: var(--text); }
.nav-item.active { background: var(--blue); background: rgba(59,130,246,0.15); color: var(--blue); font-weight: 500; }
.nav-item .icon { width: 18px; text-align: center; font-size: 15px; display: flex; align-items: center; justify-content: center; }
.nav-item i[data-lucide], .btn i[data-lucide] { width: 16px; height: 16px; }
.file-remove i[data-lucide] { width: 14px; height: 14px; }
.nav-badge { margin-left: auto; background: var(--blue); color: #fff; font-size: 11px; padding: 1px 6px; border-radius: 10px; }
.sidebar-footer { padding: 16px 20px; border-top: 1px solid var(--border); }
.agent-status { font-size: 12px; display: flex; align-items: center; gap: 6px; }
.agent-dot { width: 7px; height: 7px; border-radius: 50%; }
.agent-dot.online { background: var(--green); }
.agent-dot.offline { background: var(--red); }

/* Main content */
.main { margin-left: 240px; flex: 1; padding: 32px; max-width: 1000px; }
.page { display: none; }
.page.active { display: block; }
.page-header { margin-bottom: 24px; }
.page-header h1 { font-size: 24px; font-weight: 600; margin-bottom: 4px; }
.page-header p { color: var(--text3); font-size: 14px; }

/* Cards */
.card { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius); padding: 20px; margin-bottom: 12px; }
.card:hover { border-color: #333; }
.card-row { display: flex; justify-content: space-between; align-items: center; }
.card h3 { font-size: 15px; font-weight: 500; }
.card .meta { font-size: 13px; color: var(--text3); margin-top: 4px; }

/* Buttons */
.btn { padding: 9px 18px; border-radius: 8px; border: none; cursor: pointer; font-size: 13px; font-weight: 500; transition: all 0.1s; display: inline-flex; align-items: center; gap: 6px; }
.btn-primary { background: var(--blue); color: #fff; }
.btn-primary:hover { background: var(--blue2); }
.btn-ghost { background: transparent; color: var(--text2); border: 1px solid var(--border); }
.btn-ghost:hover { background: var(--surface2); color: var(--text); }

/* Send form */
.form-group { margin-bottom: 16px; }
.form-group label { display: block; font-size: 13px; color: var(--text2); margin-bottom: 6px; }
.form-input { width: 100%; padding: 10px 12px; background: var(--bg); border: 1px solid var(--border); border-radius: 8px; color: var(--text); font-size: 14px; }
.form-input:focus { outline: none; border-color: var(--blue); }
textarea.form-input { min-height: 80px; resize: vertical; }

/* Upload zone */
.upload-zone { border: 2px dashed var(--border); border-radius: 12px; padding: 40px; text-align: center; cursor: pointer; transition: all 0.15s; }
.upload-zone:hover, .upload-zone.dragover { border-color: var(--blue); background: rgba(59,130,246,0.05); }
.upload-zone h3 { margin-bottom: 4px; }
.upload-zone p { color: var(--text3); font-size: 13px; }
.upload-zone input[type=file] { display: none; }

/* File list */
.file-item { display: flex; align-items: center; gap: 12px; padding: 10px 12px; background: var(--surface2); border-radius: 8px; margin-bottom: 6px; font-size: 14px; }
.file-item .file-size { color: var(--text3); font-size: 12px; margin-left: auto; }
.file-item .file-remove { color: var(--text3); cursor: pointer; }
.file-item .file-remove:hover { color: var(--red); }

/* Badge */
.badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; }
.badge-green { background: rgba(34,197,94,0.15); color: var(--green); }
.badge-blue { background: rgba(59,130,246,0.15); color: var(--blue); }
.badge-gray { background: rgba(113,113,122,0.15); color: var(--text3); }

/* Table */
table { width: 100%; border-collapse: collapse; }
th { text-align: left; padding: 10px 12px; color: var(--text3); font-size: 12px; text-transform: uppercase; letter-spacing: 0.3px; border-bottom: 1px solid var(--border); font-weight: 500; }
td { padding: 12px; border-bottom: 1px solid var(--border); font-size: 14px; }
tr:hover td { background: var(--surface); }

/* Stats row */
.stats-row { display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; margin-bottom: 24px; }
.stat-card { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius); padding: 16px; }
.stat-card .stat-value { font-size: 28px; font-weight: 600; }
.stat-card .stat-label { font-size: 12px; color: var(--text3); margin-top: 2px; }

/* Empty state */
.empty { text-align: center; padding: 48px 24px; color: var(--text3); }
.empty h3 { color: var(--text2); margin-bottom: 4px; }

/* Modal */
.modal-bg { display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.6); z-index: 100; align-items: center; justify-content: center; }
.modal-bg.show { display: flex; }
.modal { background: var(--surface); border: 1px solid var(--border); border-radius: 14px; padding: 28px; max-width: 480px; width: 90%; }
.modal h2 { font-size: 18px; margin-bottom: 16px; }
.modal .actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 20px; }

.share-url { background: var(--bg); border: 1px solid var(--border); border-radius: 8px; padding: 12px; font-family: monospace; font-size: 13px; display: flex; align-items: center; justify-content: space-between; margin: 12px 0; }
.share-url code { color: var(--blue); word-break: break-all; }
</style>
</head>
<body>

<!-- Sidebar Navigation -->
<div class="sidebar">
    <div class="sidebar-logo"><span>up</span>down</div>
    <div class="sidebar-nav">
        <div class="nav-section">
            <div class="nav-item active" onclick="navigate('dashboard')">
                <span class="icon"><i data-lucide="layout-dashboard"></i></span> Dashboard
            </div>
        </div>
        <div class="nav-section">
            <div class="nav-section-label">Transfer</div>
            <div class="nav-item" onclick="navigate('send')">
                <span class="icon"><i data-lucide="upload"></i></span> Send Files
            </div>
            <div class="nav-item" onclick="navigate('inbox')">
                <span class="icon"><i data-lucide="inbox"></i></span> Inbox
                <span class="nav-badge" id="inbox-count" style="display:none">0</span>
            </div>
            <div class="nav-item" onclick="navigate('sent')">
                <span class="icon"><i data-lucide="check-circle"></i></span> Sent
            </div>
        </div>
        <div class="nav-section">
            <div class="nav-section-label">Share</div>
            <div class="nav-item" onclick="navigate('links')">
                <span class="icon"><i data-lucide="link"></i></span> Share Links
            </div>
            <div class="nav-item" onclick="navigate('dropboxes')">
                <span class="icon"><i data-lucide="archive"></i></span> Drop Boxes
            </div>
        </div>
        <div class="nav-section">
            <div class="nav-section-label">System</div>
            <div class="nav-item" onclick="navigate('history')">
                <span class="icon"><i data-lucide="history"></i></span> Transfer History
            </div>
        </div>
    </div>
    <div class="sidebar-footer">
        <div class="agent-status">
            <div class="agent-dot" id="agent-dot"></div>
            <span id="agent-label">Checking agent...</span>
        </div>
    </div>
</div>

<!-- Main Content -->
<div class="main">

    <!-- Dashboard -->
    <div class="page active" id="page-dashboard">
        <div class="page-header">
            <h1>Dashboard</h1>
            <p>Overview of your transfer activity</p>
        </div>
        <div class="stats-row">
            <div class="stat-card"><div class="stat-value" id="stat-sent">0</div><div class="stat-label">Packages Sent</div></div>
            <div class="stat-card"><div class="stat-value" id="stat-received">0</div><div class="stat-label">Received</div></div>
            <div class="stat-card"><div class="stat-value" id="stat-links">0</div><div class="stat-label">Share Links</div></div>
            <div class="stat-card"><div class="stat-value" id="stat-speed">--</div><div class="stat-label">Avg Speed</div></div>
        </div>
        <h2 style="font-size:16px;margin-bottom:12px">Recent Activity</h2>
        <div id="dashboard-activity"><div class="empty"><h3>No activity yet</h3><p>Send your first package to get started</p></div></div>
        <div style="margin-top:24px;text-align:center">
            <button class="btn btn-primary" onclick="navigate('send')"><i data-lucide="send"></i> Send Files</button>
        </div>
    </div>

    <!-- Send Files -->
    <div class="page" id="page-send">
        <div class="page-header">
            <h1>Send Files</h1>
            <p>Send files to one or more recipients at maximum speed</p>
        </div>
        <div class="card">
            <div class="form-group">
                <label>To (email or username, comma-separated)</label>
                <input type="text" class="form-input" id="send-to" placeholder="recipient@example.com, user2">
            </div>
            <div class="form-group">
                <label>Subject</label>
                <input type="text" class="form-input" id="send-subject" placeholder="Project deliverables">
            </div>
            <div class="form-group">
                <label>Note (optional)</label>
                <textarea class="form-input" id="send-note" placeholder="Add a message..."></textarea>
            </div>
            <div class="form-group">
                <label>Files</label>
                <div class="upload-zone" id="send-zone" onclick="document.getElementById('send-files').click()">
                    <h3>Drop files here</h3>
                    <p>or click to browse</p>
                    <input type="file" id="send-files" multiple>
                </div>
                <div id="send-file-list"></div>
            </div>
            <div style="display:flex;gap:8px;justify-content:flex-end;margin-top:16px">
                <button class="btn btn-ghost" onclick="navigate('dashboard')">Cancel</button>
                <button class="btn btn-primary" onclick="sendPackage()"><i data-lucide="send"></i> Send Package</button>
            </div>
        </div>
    </div>

    <!-- Inbox -->
    <div class="page" id="page-inbox">
        <div class="page-header">
            <h1>Inbox</h1>
            <p>Packages sent to you</p>
        </div>
        <div id="inbox-list"><div class="empty"><h3>No packages</h3><p>Packages sent to you will appear here</p></div></div>
    </div>

    <!-- Sent -->
    <div class="page" id="page-sent">
        <div class="page-header">
            <h1>Sent</h1>
            <p>Packages you've sent and their delivery status</p>
        </div>
        <div id="sent-list"><div class="empty"><h3>No sent packages</h3><p>Send your first package to see it here</p></div></div>
    </div>

    <!-- Share Links -->
    <div class="page" id="page-links">
        <div class="page-header" style="display:flex;justify-content:space-between;align-items:center">
            <div><h1>Share Links</h1><p>Manage your file sharing links</p></div>
            <button class="btn btn-primary" onclick="navigate('send')"><i data-lucide="plus"></i> New Share Link</button>
        </div>
        <div id="links-list"><div class="empty"><h3>No share links</h3><p>Create a share link when sending a package</p></div></div>
    </div>

    <!-- Drop Boxes -->
    <div class="page" id="page-dropboxes">
        <div class="page-header" style="display:flex;justify-content:space-between;align-items:center">
            <div><h1>Drop Boxes</h1><p>Public upload portals for receiving files from anyone</p></div>
            <button class="btn btn-primary" onclick="createDropbox()"><i data-lucide="plus"></i> Create Drop Box</button>
        </div>
        <div id="dropbox-list"><div class="empty"><h3>No drop boxes</h3><p>Create a drop box to let external users send you files</p></div></div>
    </div>

    <!-- Transfer History -->
    <div class="page" id="page-history">
        <div class="page-header">
            <h1>Transfer History</h1>
            <p>Complete log of all file transfers</p>
        </div>
        <table>
            <thead><tr><th>File</th><th>Size</th><th>Speed</th><th>Duration</th><th>Status</th><th>Date</th></tr></thead>
            <tbody id="history-body"><tr><td colspan="6" class="empty">No transfers yet</td></tr></tbody>
        </table>
    </div>

</div>

<script>
const API = '';
const AGENT = 'http://127.0.0.1:19876';

// Session check — redirect to login if not authenticated
fetch('/api/me').then(r => { if (!r.ok) window.location = '/login'; });
let selectedFiles = [];

// Navigation
function navigate(page) {
    document.querySelectorAll('.page').forEach(p => p.classList.remove('active'));
    document.querySelectorAll('.nav-item').forEach(n => n.classList.remove('active'));
    document.getElementById('page-' + page).classList.add('active');
    event?.target?.closest?.('.nav-item')?.classList.add('active');

    if (page === 'inbox') loadInbox();
    if (page === 'sent') loadSent();
    if (page === 'history') loadHistory();
    if (page === 'links') loadLinks();
}

// Agent detection
async function checkAgent() {
    try {
        const r = await fetch(AGENT + '/status', { signal: AbortSignal.timeout(1000) });
        document.getElementById('agent-dot').classList.add('online');
        document.getElementById('agent-label').textContent = 'Agent running';
    } catch(e) {
        document.getElementById('agent-dot').classList.add('offline');
        document.getElementById('agent-label').textContent = 'Agent offline';
    }
}
checkAgent();

// File handling for Send
const sendZone = document.getElementById('send-zone');
const sendInput = document.getElementById('send-files');
sendZone.addEventListener('dragover', e => { e.preventDefault(); sendZone.classList.add('dragover'); });
sendZone.addEventListener('dragleave', () => sendZone.classList.remove('dragover'));
sendZone.addEventListener('drop', e => { e.preventDefault(); sendZone.classList.remove('dragover'); addFiles(e.dataTransfer.files); });
sendInput.addEventListener('change', () => addFiles(sendInput.files));

function addFiles(files) {
    for (const f of files) selectedFiles.push(f);
    renderFileList();
}
function removeFile(idx) {
    selectedFiles.splice(idx, 1);
    renderFileList();
}
function renderFileList() {
    const el = document.getElementById('send-file-list');
    if (!selectedFiles.length) { el.innerHTML = ''; return; }
    el.innerHTML = selectedFiles.map((f, i) =>
        '<div class="file-item"><span>' + f.name + '</span><span class="file-size">' + formatBytes(f.size) + '</span><span class="file-remove" onclick="removeFile(' + i + ')"><i data-lucide="x"></i></span></div>'
    ).join('');
    lucide.createIcons();
}

// Send package
async function sendPackage() {
    if (!selectedFiles.length) return alert('Add files first');
    const to = document.getElementById('send-to').value;
    const subject = document.getElementById('send-subject').value || 'Untitled Package';

    const formData = new FormData();
    selectedFiles.forEach(f => formData.append('files', f));

    try {
        const resp = await fetch(API + '/api/upload', { method: 'POST', body: formData });
        const data = await resp.json();
        if (data.ok) {
            // Create share link
            const shareResp = await fetch(API + '/api/share', {
                method: 'POST',
                headers: {'Content-Type':'application/json'},
                body: JSON.stringify({ package_id: data.data.package_id })
            });
            const shareData = await shareResp.json();
            selectedFiles = [];
            renderFileList();
            document.getElementById('send-to').value = '';
            document.getElementById('send-subject').value = '';
            document.getElementById('send-note').value = '';
            alert('Package sent! Share link: ' + (shareData.ok ? shareData.data.url : 'created'));
            navigate('sent');
        }
    } catch(e) { alert('Send failed: ' + e); }
}

// Load pages
async function loadInbox() {
    // For now, show packages (in a real system, filtered by recipient)
    const resp = await fetch(API + '/api/packages');
    const data = await resp.json();
    const el = document.getElementById('inbox-list');
    if (!data.ok || !data.data.length) { el.innerHTML = '<div class="empty"><h3>No packages</h3></div>'; return; }
    el.innerHTML = data.data.map(p => `
        <div class="card" style="cursor:pointer">
            <div class="card-row">
                <div>
                    <h3>${p.name}</h3>
                    <div class="meta">${p.files.length} file(s) &middot; ${formatBytes(p.total_size)} &middot; ${p.created_at.slice(0,10)}</div>
                </div>
                <button class="btn btn-primary" onclick="downloadPackage('${p.id}')"><i data-lucide="download"></i> Download</button>
            </div>
        </div>
    `).join('');
    lucide.createIcons();
}

async function loadSent() {
    const resp = await fetch(API + '/api/packages');
    const data = await resp.json();
    const el = document.getElementById('sent-list');
    if (!data.ok || !data.data.length) { el.innerHTML = '<div class="empty"><h3>No sent packages</h3></div>'; return; }
    el.innerHTML = data.data.map(p => `
        <div class="card">
            <div class="card-row">
                <div>
                    <h3>${p.name}</h3>
                    <div class="meta">${p.files.length} file(s) &middot; ${formatBytes(p.total_size)}</div>
                </div>
                <span class="badge badge-green">Delivered</span>
            </div>
        </div>
    `).join('');
}

async function loadHistory() {
    const resp = await fetch(API + '/api/transfers');
    const data = await resp.json();
    const body = document.getElementById('history-body');
    if (!data.ok || !data.data.length) { body.innerHTML = '<tr><td colspan="6" class="empty">No transfers</td></tr>'; return; }
    body.innerHTML = data.data.map(t => `<tr>
        <td>${t.filename}</td>
        <td>${formatBytes(t.file_size)}</td>
        <td>${t.rate_mbps > 0 ? t.rate_mbps.toFixed(0) + ' Mbps' : '--'}</td>
        <td>${t.duration_ms > 0 ? (t.duration_ms/1000).toFixed(1) + 's' : '--'}</td>
        <td><span class="badge badge-${t.status==='completed'?'green':'gray'}">${t.status}</span></td>
        <td style="color:var(--text3)">${t.created_at.slice(0,16).replace('T',' ')}</td>
    </tr>`).join('');
}

async function loadLinks() {
    // Would need a list_share_links API endpoint
    document.getElementById('links-list').innerHTML = '<div class="empty"><h3>Coming soon</h3></div>';
}

function downloadPackage(id) {
    window.location = 'updown://download?package=' + id + '&server=' + location.host;
}
function createDropbox() {
    alert('Drop box creation coming soon');
}

function formatBytes(b) {
    if (b >= 1073741824) return (b/1073741824).toFixed(1) + ' GB';
    if (b >= 1048576) return (b/1048576).toFixed(1) + ' MB';
    if (b >= 1024) return (b/1024).toFixed(1) + ' KB';
    return b + ' B';
}

// Load dashboard stats
fetch(API + '/api/health').then(r=>r.json()).then(d => {
    if (d.ok) document.getElementById('stat-sent').textContent = d.data.transfers_completed;
});

// Initialize Lucide icons
document.addEventListener('DOMContentLoaded', () => { lucide.createIcons(); });
</script>
</body>
</html>"##;

const DOWNLOAD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>updown — Download</title>
<style>
:root { --bg: #09090b; --surface: #131316; --border: #27272a; --text: #e4e4e7; --text3: #71717a; --blue: #3b82f6; }
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: var(--bg); color: var(--text); min-height: 100vh; display: flex; align-items: center; justify-content: center; }
.card { background: var(--surface); border: 1px solid var(--border); border-radius: 16px; padding: 48px; max-width: 480px; width: 90%; text-align: center; }
h1 { font-size: 26px; margin-bottom: 4px; }
h1 span { color: var(--blue); }
.file-info { margin: 28px 0; }
.file-name { font-size: 18px; margin-bottom: 4px; }
.file-meta { color: var(--text3); font-size: 13px; }
.btn { display: inline-block; padding: 14px 32px; border-radius: 8px; border: none; cursor: pointer; font-size: 15px; font-weight: 600; background: var(--blue); color: #fff; }
.btn:hover { background: #2563eb; }
.alt { margin-top: 16px; font-size: 12px; color: var(--text3); }
.alt a { color: var(--blue); }
</style>
</head>
<body>
<div class="card">
    <h1><span>up</span>down</h1>
    <div class="file-info">
        <div class="file-name" id="dl-name">Loading...</div>
        <div class="file-meta" id="dl-meta"></div>
    </div>
    <button class="btn" onclick="startDownload()">Download</button>
    <p class="alt">Requires the <a href="#">updown agent</a> for maximum speed.<br>Or <a href="#" id="dl-http">download via browser</a> (slower).</p>
</div>
<script>
const code = location.pathname.split('/').pop();
fetch('/api/share/' + code).then(r=>r.json()).then(d => {
    if (d.ok) {
        document.getElementById('dl-name').textContent = 'Package ' + d.data.package_id.slice(0,8);
        document.getElementById('dl-meta').textContent = (d.data.download_count || 0) + ' downloads';
    } else {
        document.getElementById('dl-name').textContent = 'Link expired or not found';
        document.querySelector('.btn').style.display = 'none';
    }
});
function startDownload() {
    window.location = 'updown://download?code=' + code + '&server=' + location.host;
}
</script>
</body>
</html>"##;

const SUBMIT_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>updown — Submit Files</title>
<style>
:root { --bg: #09090b; --surface: #131316; --border: #27272a; --text: #e4e4e7; --text3: #71717a; --blue: #3b82f6; }
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: var(--bg); color: var(--text); min-height: 100vh; display: flex; align-items: center; justify-content: center; }
.card { background: var(--surface); border: 1px solid var(--border); border-radius: 16px; padding: 48px; max-width: 560px; width: 90%; }
h1 { font-size: 22px; margin-bottom: 4px; }
h1 span { color: var(--blue); }
.subtitle { color: var(--text3); font-size: 14px; margin-bottom: 28px; }
.upload-zone { border: 2px dashed var(--border); border-radius: 12px; padding: 40px; text-align: center; cursor: pointer; margin-bottom: 16px; }
.upload-zone:hover { border-color: var(--blue); }
.upload-zone h3 { margin-bottom: 4px; }
.upload-zone p { color: var(--text3); font-size: 13px; }
.upload-zone input { display: none; }
.form-input { width: 100%; padding: 10px 12px; background: var(--bg); border: 1px solid var(--border); border-radius: 8px; color: var(--text); font-size: 14px; margin-bottom: 12px; }
.btn { padding: 12px 24px; border-radius: 8px; border: none; cursor: pointer; font-size: 14px; font-weight: 600; background: var(--blue); color: #fff; width: 100%; }
.btn:hover { background: #2563eb; }
#result { margin-top: 16px; text-align: center; }
</style>
</head>
<body>
<div class="card">
    <h1><span>up</span>down</h1>
    <p class="subtitle">Submit files to this drop box</p>
    <div class="upload-zone" onclick="document.getElementById('submit-files').click()">
        <h3>Drop files here</h3>
        <p>or click to browse</p>
        <input type="file" id="submit-files" multiple>
    </div>
    <input type="email" class="form-input" id="submit-email" placeholder="Your email (optional)">
    <input type="text" class="form-input" id="submit-note" placeholder="Note (optional)">
    <button class="btn" onclick="submitFiles()">Submit</button>
    <div id="result"></div>
</div>
<script>
let files = [];
const zone = document.querySelector('.upload-zone');
const input = document.getElementById('submit-files');
zone.addEventListener('dragover', e => { e.preventDefault(); });
zone.addEventListener('drop', e => { e.preventDefault(); files = [...e.dataTransfer.files]; zone.querySelector('h3').textContent = files.length + ' file(s) selected'; });
input.addEventListener('change', () => { files = [...input.files]; zone.querySelector('h3').textContent = files.length + ' file(s) selected'; });

async function submitFiles() {
    if (!files.length) return alert('Select files first');
    const fd = new FormData();
    files.forEach(f => fd.append('files', f));
    const resp = await fetch('/api/upload', { method: 'POST', body: fd });
    const data = await resp.json();
    document.getElementById('result').innerHTML = data.ok
        ? '<p style="color:#22c55e">Submitted successfully!</p>'
        : '<p style="color:#ef4444">Submission failed</p>';
}
</script>
</body>
</html>"##;

const LOGIN_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>updown — Login</title>
<style>
:root { --bg: #09090b; --surface: #131316; --border: #27272a; --text: #e4e4e7; --text3: #71717a; --blue: #3b82f6; --red: #ef4444; }
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: var(--bg); color: var(--text); min-height: 100vh; display: flex; align-items: center; justify-content: center; }
.login-card { background: var(--surface); border: 1px solid var(--border); border-radius: 16px; padding: 48px; max-width: 400px; width: 90%; }
h1 { font-size: 28px; text-align: center; margin-bottom: 4px; }
h1 span { color: var(--blue); }
.subtitle { text-align: center; color: var(--text3); font-size: 14px; margin-bottom: 32px; }
.form-group { margin-bottom: 16px; }
.form-group label { display: block; font-size: 13px; color: var(--text3); margin-bottom: 6px; }
.form-input { width: 100%; padding: 12px; background: var(--bg); border: 1px solid var(--border); border-radius: 8px; color: var(--text); font-size: 14px; }
.form-input:focus { outline: none; border-color: var(--blue); }
.btn { width: 100%; padding: 12px; border-radius: 8px; border: none; cursor: pointer; font-size: 15px; font-weight: 600; background: var(--blue); color: #fff; margin-top: 8px; }
.btn:hover { background: #2563eb; }
.error { color: var(--red); font-size: 13px; margin-top: 12px; text-align: center; display: none; }
.help { margin-top: 20px; font-size: 12px; color: var(--text3); text-align: center; }
.help code { background: var(--bg); padding: 2px 6px; border-radius: 4px; font-size: 11px; }
</style>
</head>
<body>
<div class="login-card">
    <h1><span>up</span>down</h1>
    <p class="subtitle">Sign in to your account</p>
    <form onsubmit="doLogin(event)">
        <div class="form-group">
            <label>API Key</label>
            <input type="password" class="form-input" id="api-key" placeholder="upd_..." autofocus>
        </div>
        <button type="submit" class="btn">Sign In</button>
    </form>
    <p class="error" id="error-msg"></p>
    <p class="help">Your API key was shown when the server started.<br>Look for <code>api_key=upd_...</code> in the server logs.</p>
</div>
<script>
fetch('/api/me').then(r => { if (r.ok) window.location = '/'; });
async function doLogin(e) {
    e.preventDefault();
    const key = document.getElementById('api-key').value.trim();
    if (!key) return;
    const r = await fetch('/api/login', { method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({api_key:key}) });
    const data = await r.json();
    if (data.ok) { window.location = '/'; }
    else { document.getElementById('error-msg').textContent = data.error||'Invalid API key'; document.getElementById('error-msg').style.display='block'; }
}
</script>
</body>
</html>"##;
