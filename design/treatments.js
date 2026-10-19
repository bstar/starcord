'use strict';

// One pane renderer for all ten studies. Only shared roles and composition vary.
const treatments = [
  {id:'classic', name:'Classic rack', cue:'AMP / original craft', direction:'The closest relative to AMP.', summary:'Raised frames, dark inset wells and compact title strips. The original family resemblance.', rationale:'Bring the Classic Rack construction to every CORD module. Servers, chat, composer and members all share the same raised shell and recessed interior.', construction:'Style::panel + Style::frame, with the default 38 px title allocation.', content:'KIT Classic inset roles. Compact 12 px messages; 11 px captions.', tradeoff:'Strong character; repeated bevels make a tall accordion feel busier.'},
  {id:'rigid', name:'Rigid instrument', cue:'AMP / square chassis', direction:'A more architectural rack.', summary:'Square corners and stronger seams give the five modules a precise, assembled feel.', rationale:'Use KIT’s rigid panel frame throughout. Flat edges line up the stack; a stronger title rule makes each module easy to locate without changing its behavior.', construction:'Style::frame_at_density with rigid enabled and one shared title recipe.', content:'The same recessed well in every pane. Square rows and controls.', tradeoff:'The heaviest chrome of the set. Best for people who like a visible chassis.'},
  {id:'workspace', name:'Quiet workspace', cue:'REEL / shared workspace', direction:'Let the conversation lead.', summary:'Fine separators, shallow controls and neutral surfaces borrowed directly from KIT’s workspace vocabulary.', rationale:'Apply REEL’s quiet workspace construction to the entire accordion. Content has room to breathe while a single accent identifies selection and focus.', construction:'Style::workspace_panel + Style::tool. Shared 32 px headers.', content:'WorkspaceColors::well throughout; Liberation Sans labels and Mono metadata.', tradeoff:'Less of AMP’s tactile character, but the most direct match for REEL’s dense workspaces.'},
  {id:'inset', name:'Deep wells', cue:'FOLD / recessed content', direction:'Content sits inside the instrument.', summary:'Generous inset lips separate the shared panel shell from a deep, uninterrupted reading surface.', rationale:'Emphasize FOLD’s division between frame and content. Every module gets the same double inset edge, keeping the message list and composer visibly related.', construction:'Style::panel + inset Style::frame, using shared padding and gap metrics.', content:'A two-pixel inset with a fine inner rule; no individual message cards.', tradeoff:'More edge and padding consume vertical room. Better at comfortable window sizes.'},
  {id:'ledger', name:'Conversation ledger', cue:'REEL / aligned information', direction:'A conversation you can scan.', summary:'Continuous workspace seams, ruled messages and aligned times give chat the clarity of a working ledger.', rationale:'Make information alignment the organizing idea. The frame stays quiet, the message baseline is regular, and time sits at the far edge of each author line.', construction:'Style::workspace_panel; zero inter-pane gap with shared separator roles.', content:'Ruled message rows, column-aligned time, flat list selection.', tradeoff:'Excellent scanning; long conversations can feel more structured than social.'},
  {id:'minimal', name:'Soft-spoken', cue:'KIT / minimum chrome', direction:'Reduce the frame to its essentials.', summary:'A nearly continuous neutral canvas, with just enough division to keep the accordion legible.', rationale:'Keep the same KIT workspace roles but remove extra raised edges. Text initials replace avatar blocks. Color appears where it carries state.', construction:'Workspace surfaces and sections; minimal visible outer borders.', content:'The workspace panel role continues through the content, with small neutral author marks.', tradeoff:'Quietest appearance; module boundaries are subtler than the other options.'},
  {id:'focus', name:'Focus signal', cue:'KIT / explicit focus', direction:'Always know where keys will land.', summary:'Neutral workspace panels with a clear accent outline and title fill on the active module.', rationale:'Make keyboard focus the strongest signal, independent of selected rows. Clicking another module moves the signal, while channels retain their own selected state.', construction:'Style::workspace_panel + Style::focus; a two-pixel application focus outline.', content:'Shared workspace wells. Only the focused module gets an accented title treatment.', tradeoff:'Clear for keyboard use; the focus outline has more presence than Classic’s subtle edge.'},
  {id:'airy', name:'Reading room', cue:'FOLD / generous rhythm', direction:'The comfortable long-session treatment.', summary:'Classic construction with more room around text, controls and pane seams.', rationale:'Keep the family’s tactile shell but choose a more relaxed spacing scale. The spacing increase applies to every pane, instead of making chat a separate visual system.', construction:'Style::panel and inset frames; 12 px outer gaps and 18 px content gutters.', content:'More leading and space between author groups; unchanged type and palette.', tradeoff:'Uses the most vertical space. The layout must give spacing back on short viewports.'},
  {id:'contrast', name:'Clear signal', cue:'KIT / strong boundaries', direction:'A crisp, legible instrument.', summary:'Square outlines, foreground metadata and simple flat wells make boundaries and states unmistakable.', rationale:'Use existing foreground, border and accent roles for clear hierarchy. Labels, timestamps and status stay fully visible without ornamental surface changes.', construction:'Shared flat surfaces, explicit border roles and two-pixel focus.', content:'Foreground metadata and a thicker selected-row marker. State uses shape as well as color.', tradeoff:'More metadata competes with message text. This is a visual proposal, not an accessibility certification.'},
  {id:'family', name:'Family standard', cue:'AMP + FOLD + REEL', direction:'One recipe for every content pane.', summary:'Classic outer frames, REEL’s quiet title strips, FOLD’s inset discipline. A combination of the family’s existing primitives.', rationale:'Keep the recognizable STAR shell, then use one quiet workspace well for all five modules. Fine message rules add structure without giving chat a different kind of container.', construction:'Style::panel outside; WorkspaceColors and shared controls inside. One reusable CORD pane adapter.', content:'Consistent shallow wells, 32 px headers, 12 px gutters and small metadata. Accent only for state.', tradeoff:'A composition of existing KIT primitives. The common pane adapter must own the recipe so modules cannot drift.'},
];

let current = 6;
let expanded = 'channels';
let focused = 'conversation';
let reader = false;
let theme = 'kanagawa';
let sample = 'conversation';
let saved = new Set();
const drafts = Object.create(null);
try {
  const parsed = JSON.parse(localStorage.getItem('starcord-design-shortlist') || '[]');
  if (Array.isArray(parsed)) saved = new Set(parsed.filter(id => treatments.some(t => t.id === id)));
} catch { /* Local-file or private browsing may disable storage. */ }
const $ = selector => document.querySelector(selector);
const escapeHtml = value => String(value).replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));

function message(initials, name, time, body, extra = '') {
  return `<article class="message ${extra}"><span class="avatar" aria-hidden="true">${initials}</span><div><header><strong>${name}</strong><time>${time}</time></header>${body}</div></article>`;
}
function messages(state) {
  if (state === 'empty') return '<div class="empty-channel"><strong># quiet-corner</strong><p>A little room for a new conversation.<br>No messages here yet.</p></div>';
  let html = '<div class="date-line">SUNDAY / OCTOBER 18</div>';
  html += message('MK','Mika','10:42','<p>Morning. The new pane treatment is looking good.<br>Can we keep the content surfaces consistent across the family?</p>');
  if (state === 'unread') html += '<div class="unread-line">2 UNREAD MESSAGES</div>';
  html += message('JN','Jules','10:44',`<p>${state === 'unread' ? '<span class="accent">@bstar</span> ' : ''}Yes. Same frame, same well, same focus behavior.<br>The conversation should feel like it belongs next to AMP and FOLD.</p>`,state === 'unread' ? 'mention-message' : '');
  const attachment = state === 'reply' ? '<div class="attachment"><span aria-hidden="true">▧</span><div>pane-study.png<small>Image attachment · 248 KB</small></div></div>' : '';
  html += message('BS','bstar','10:46','<p>Exactly. Let’s use the shared <code>Style</code> and keep the chrome in KIT.</p>'+attachment+'<div class="reactions"><button type="button" data-reaction aria-pressed="false" aria-label="React with plus one">+1 · 3</button><button type="button" data-reaction aria-pressed="false" aria-label="React with star">☆ · 2</button></div>');
  return html + '<div class="typing"><b>···</b> Mika is typing</div>';
}
function pane(id, title, detail, body, actions = '', isList = false) {
  const open = expanded === id;
  return `<section class="pane ${focused === id ? 'focused' : ''} ${open ? 'expanded' : ''} ${id}" data-pane="${id}" aria-label="${title}"><header class="pane-head"><button type="button" class="pane-toggle" data-focus="${id}" ${isList ? `aria-expanded="${open}"` : ''}>${isList ? (open ? '▾ ' : '▸ ') : ''}${title}</button><span class="pane-detail">${detail}</span><div class="pane-actions">${actions}</div></header><div class="pane-body">${body}</div></section>`;
}
function appMarkup(treatment, miniature = false) {
  const room = sample === 'empty' ? 'quiet-corner' : 'design-room';
  const servers = '<div class="summary"><span class="server-icon">S/</span><span>STAR collective</span><span class="right">3 servers · 2 unread</span></div><div class="server-list"><div class="list-row selected">STAR collective <span>12</span></div><div class="list-row">The listening room <span>2</span></div><div class="list-row">Workshop <span>—</span></div></div>';
  const channels = `<div class="summary"><span class="accent">#</span><span>${room}</span><span class="right">12 text channels</span></div><div class="channel-list"><div class="list-row"># general <span>2</span></div><div class="list-row selected"># ${room} <span>${sample === 'unread' ? '2 new' : 'SELECTED'}</span></div><div class="list-row"># releases <span>1</span></div></div>`;
  const members = '<div class="summary"><span class="accent">●</span><span>Mika, Jules, bstar + 5</span><span class="right">8 online · 24 members</span></div><div class="member-list"><div class="list-row">● Mika <span>online</span></div><div class="list-row">● Jules <span>online</span></div><div class="list-row">● bstar <span>online</span></div></div>';
  const draft = drafts[room] || '';
  const composer = (sample === 'reply' ? '<div class="reply-context">Replying to Mika · Can we keep the content surfaces consistent?</div>' : '') + `<textarea aria-label="Message draft for ${room}" placeholder="Message #${room}" ${miniature ? 'tabindex="-1"' : ''}>${escapeHtml(draft)}</textarea><div class="composer-meta"><span>Enter send · Shift+Enter newline</span><span>Markdown supported</span></div><div class="sample-notice" role="status"></div>`;
  const action = (text, name) => `<button type="button" data-demo="${name}">${text}</button>`;
  return `<div class="app ${reader ? 'reader' : ''}" data-treatment="${treatment.id}" data-theme="${theme}"><header class="appbar"><span class="app-brand">STAR<i>/</i>CORD</span><span class="connection">Connected · bstar</span></header>${pane('servers','SERVERS','01',servers,action('Settings','settings'),true)}${pane('channels','CHANNELS','02',channels,'',true)}${pane('conversation','CONVERSATION','# '+room,messages(sample),action('Search','search')+action('Pins','pins'))}${pane('composer','COMPOSE','04',composer,action('Attach','attach')+action('Emoji','emoji'))}${pane('members','MEMBERS','05',members,'',true)}<footer class="app-status"><span class="accent">● ONLINE</span><span>STAR collective / # ${room}</span><span>Tab controls · click titles to fold</span></footer></div>`;
}
function applyPalettes() {
  const palette = KIT_PALETTES[theme];
  for (const key of ['bg','fg','dim','accent','selected','border','shadow','wp','wh','line','control','hover']) {
    document.documentElement.style.setProperty('--review-'+key, palette[key]);
  }
  document.documentElement.style.colorScheme = theme === 'latte' ? 'light' : 'dark';
  document.querySelectorAll('.app').forEach(app => {
    Object.entries(KIT_PALETTES[theme]).forEach(([key,value]) => app.style.setProperty('--'+key,value));
    app.style.colorScheme = theme === 'latte' ? 'light' : 'dark';
  });
}
function renderPreview() {
  $('#preview').innerHTML = appMarkup(treatments[current]);
  applyPalettes();
}
function renderOverview() {
  const onlySaved = $('#shortlist-only').checked;
  $('#overview-grid').innerHTML = treatments.map((t,i) => onlySaved && !saved.has(t.id) ? '' : `<article class="overview-card"><button type="button" class="overview-link" data-choose="${i}"><span>${String(i+1).padStart(2,'0')}</span>${t.name}${saved.has(t.id)?'<small>★ Shortlisted</small>':''}</button><div class="thumbnail" aria-hidden="true" inert>${appMarkup(t,true)}</div><p class="overview-caption">${t.summary}</p></article>`).join('');
  $('#empty-shortlist').hidden = !onlySaved || saved.size > 0;
  scaleThumbnails();
  applyPalettes();
}
function scaleThumbnails() {
  document.querySelectorAll('.thumbnail').forEach(container => {
    const app = container.firstElementChild;
    const scale = container.clientWidth / 940;
    app.style.transform = `scale(${scale})`;
    container.style.height = `${Math.ceil(app.offsetHeight * scale)}px`;
  });
}
function updateSelection() {
  const t = treatments[current];
  $('#treatment-number').textContent = `TREATMENT ${String(current+1).padStart(2,'0')} / 10`;
  $('#treatment-title').textContent = t.name;
  $('#treatment-summary').textContent = t.summary;
  $('#direction').textContent = t.direction;
  $('#rationale').textContent = t.rationale;
  $('#construction').textContent = t.construction;
  $('#content-rule').textContent = t.content;
  $('#tradeoff').textContent = t.tradeoff;
  document.querySelectorAll('#treatment-nav button').forEach((button,i) => button.setAttribute('aria-current', String(i === current)));
  const isSaved = saved.has(t.id);
  $('#save-choice').setAttribute('aria-pressed',String(isSaved));
  $('#save-choice').textContent = isSaved ? '★ Shortlisted' : '☆ Shortlist';
  renderPreview();
}
function choose(index, scroll = false) {
  if (!Number.isInteger(index) || index < 0 || index >= treatments.length) return;
  current = index;
  history.replaceState(null,'',`#treatment-${index+1}`);
  updateSelection();
  if (scroll) $('.selection-heading').scrollIntoView({behavior:'smooth',block:'start'});
}
function chooseFromHash() {
  const match = location.hash.match(/^#treatment-(10|[1-9])$/);
  if (match) current = Number(match[1])-1;
  updateSelection();
}
$('#treatment-nav').innerHTML = treatments.map((t,i) => `<button type="button" data-choose="${i}" aria-current="false"><span class="nav-number">${String(i+1).padStart(2,'0')}</span><span><span class="nav-name">${t.name}</span><span class="nav-description">${t.cue}</span></span></button>`).join('');
document.addEventListener('click',event => {
  const button = event.target.closest('[data-choose]');
  if (button) choose(Number(button.dataset.choose),!!button.closest('.overview'));
});
$('#recommended').addEventListener('click',()=>choose(6,true));
$('#theme').addEventListener('change',event => {theme=event.target.value;renderPreview();renderOverview();});
$('#sample-state').addEventListener('change',event => {sample=event.target.value;renderPreview();renderOverview();});
$('#reader-toggle').addEventListener('click',event => {reader=!reader;event.currentTarget.setAttribute('aria-pressed',String(reader));renderPreview();renderOverview();});
$('#save-choice').addEventListener('click',()=>{
  const id=treatments[current].id;
  if(saved.has(id)) saved.delete(id); else saved.add(id);
  try{localStorage.setItem('starcord-design-shortlist',JSON.stringify([...saved]));}catch{}
  updateSelection();renderOverview();
});
$('#shortlist-only').addEventListener('change',renderOverview);
$('#preview').addEventListener('click',event=>{
  const focus = event.target.closest('[data-focus]');
  if(focus){
    const id=focus.dataset.focus;
    if(['servers','channels','members'].includes(id)){
      expanded = expanded===id ? null : id;
      focused = expanded===id ? id : 'conversation';
    }else focused=id;
    renderPreview();renderOverview();
    $(`#preview [data-focus="${id}"]`).focus({preventScroll:true});
    return;
  }
  const reaction=event.target.closest('[data-reaction]');
  if(reaction){
    const active=reaction.getAttribute('aria-pressed')==='true';
    reaction.setAttribute('aria-pressed',String(!active));
    reaction.textContent=reaction.textContent.replace(/\d+$/,n=>Number(n)+(active?-1:1));
    return;
  }
  const demo=event.target.closest('[data-demo]');
  if(demo){
    const notice=$('#preview .sample-notice');
    const labels={settings:'Settings preview: use the theme selector above.',search:'Search preview: sample conversation shown. No network requests.',pins:'Pinned message: “Same frame, same well, same focus behavior.”',attach:'Attachment preview: choose “Reply & attachment” above.',emoji:'Emoji preview: type a shortcode such as :star: in the sample draft.'};
    notice.textContent=labels[demo.dataset.demo];
  }
});
$('#preview').addEventListener('focusin',event=>{
  const target=event.target.closest('.pane');
  if(!target || focused===target.dataset.pane) return;
  focused=target.dataset.pane;
  // Moving DOM focus must never destroy a draft or the current control.
  document.querySelectorAll('#preview .pane').forEach(p=>p.classList.toggle('focused',p===target));
});
$('#preview').addEventListener('input',event=>{
  if(event.target.matches('textarea')) drafts[sample==='empty'?'quiet-corner':'design-room']=event.target.value;
});
$('#preview').addEventListener('keydown',event=>{
  if(event.target.matches('textarea') && event.key==='Enter' && !event.shiftKey){
    event.preventDefault();
    $('#preview .sample-notice').textContent='Draft kept locally in this preview. No message was sent.';
  }
});
$('#treatment-nav').addEventListener('keydown',event=>{
  if(!['ArrowLeft','ArrowRight','ArrowUp','ArrowDown'].includes(event.key)) return;
  event.preventDefault();
  const cols=window.matchMedia('(max-width:700px)').matches?2:5;
  const step=event.key==='ArrowRight'?1:event.key==='ArrowLeft'?-1:event.key==='ArrowDown'?cols:-cols;
  choose((current+step+10)%10);
  document.querySelectorAll('#treatment-nav button')[current].focus();
});
window.addEventListener('hashchange',chooseFromHash);
window.addEventListener('resize',scaleThumbnails);
document.fonts.ready.then(scaleThumbnails);
chooseFromHash();
renderOverview();
