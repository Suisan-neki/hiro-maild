const $ = id => document.getElementById(id);
let state, submitting = false, lastError = '';
const labels = {pending:'転送待ち',retry:'再試行待ち',blocked:'保留',unknown:'結果不明',sending:'送信中',sent:'転送済み'};
const when = text => text ? new Date(text).toLocaleString('ja-JP') : '指定なし';
const text = (parent, tag, value) => { const node=document.createElement(tag); node.textContent=value; parent.append(node); return node; };
async function act(payload) {
  if (submitting || state?.busy) return;
  submitting=true; render();
  try {
    const response=await fetch('/api/action',{method:'POST',headers:{'Content-Type':'application/json','X-CSRF-Token':state.csrf},body:JSON.stringify(payload)});
    if (!response.ok) throw new Error('操作を開始できません。画面を再読み込みしてください。');
    lastError='';
  } catch(e) { lastError=e.message; }
  finally { submitting=false; await refresh(); }
}
function render() {
  const busy=submitting || state.busy;
  $('notice').textContent=lastError || state.message || '保存先と開始位置を設定し、まず送信なしで確認してください。';
  $('store-state').textContent=state.store ? `保存先：${state.store}` : '大学のThunderbird保存先は未設定です。';
  if (!$('store').value && state.store) $('store').value=state.store;
  $('stores').replaceChildren();
  for (const value of state.stores) { const option=document.createElement('option'); option.value=value; $('stores').append(option); }
  if (state.start) $('gmail').value=state.start.gmail;
  $('gmail').disabled=busy || !!state.start;
  $('store').disabled=busy || !!state.start;
  $('client').disabled=busy;
  $('configure').disabled=busy || !!state.start;
  $('initialize').disabled=busy || !!state.start || !state.store;
  $('start-controls').hidden=!!state.start;
  $('start-state').textContent=state.start ? `${when(state.start.since)} 以降・取込ID ${state.start.after_id} より後 → ${state.start.gmail}` : '未設定です。過去のメールを勝手に転送しません。';
  $('gmail-state').textContent=state.gmail ? `接続済み：${state.gmail}` : '未接続（送信なしの確認は接続前でも使えます）';
  $('auth').disabled=busy;
  $('auth-link').hidden=!state.auth_url;
  if (state.auth_url) $('auth-link').href=state.auth_url; else $('auth-link').removeAttribute('href');
  $('preview').disabled=busy || !state.start;
  $('send').disabled=busy || !state.start || !state.previewed || state.gmail!==state.start.gmail;
  $('targets').replaceChildren();
  if (!state.targets.length) text($('targets'),'p','転送対象はありません。');
  for (const {mail,mime_valid} of state.targets) {
    const row=text($('targets'),'article','');
    text(row,'strong',mail.subject);
    text(row,'p',`${labels[mail.status]} · ${mail.sender || '送信者不明'} · ${when(mail.original_date)}`);
    text(row,'p',`添付：${mail.attachment_names.join('、') || 'なし'} · 原本MIME：${mime_valid===null ? '未確認（対象確認ボタンで検証）' : mime_valid ? '確認済み' : '未保存・不正・サイズ超過（送信不可）'}`);
  }
  $('history').replaceChildren();
  if (!state.history.length) text($('history'),'p','転送履歴はまだありません。');
  for (const mail of state.history) {
    const row=text($('history'),'article','');
    text(row,'strong',mail.subject);
    text(row,'p',`${labels[mail.status]}${mail.error ? ` · ${mail.error}` : ''}`);
    if (mail.status==='unknown') {
      text(row,'p','Gmailで受信済みか確認：');
      text(row,'code',`in:anywhere rfc822msgid:${mail.forward_id.replace(/^<|>$/g,'')}`);
    }
    if (['retry','blocked','unknown'].includes(mail.status)) {
      const button=text(row,'button','再試行を予約（送信しない）'); button.disabled=busy;
      button.onclick=()=>{
        const unknown=mail.status==='unknown';
        if (unknown && !confirm('Gmailで受信済みか確認しましたか？再送すると重複する可能性があります。了承して再試行を予約しますか？')) return;
        act({op:'retry',id:mail.id,accept_duplicate_risk:unknown});
      };
    }
  }
}
async function refresh() {
  try {
    const response=await fetch('/api/status',{cache:'no-store'});
    if (!response.ok) throw new Error();
    state=await response.json(); render();
  } catch { $('notice').textContent='ローカルプロセスに接続できません。hiro-maild local-webを起動してください。'; for (const button of document.querySelectorAll('button')) button.disabled=true; }
}
$('configure').onclick=()=>act({op:'configure',store:$('store').value.trim()});
$('initialize').onclick=()=>{
  const mode=document.querySelector('input[name=mode]:checked').value;
  let since;
  if (mode==='since') { const date=new Date($('since').value); if (!Number.isFinite(date.getTime())) { $('notice').textContent='開始日時を指定してください。'; return; } since=date.toISOString(); }
  act({op:'initialize',gmail:$('gmail').value.trim(),mode,since});
};
for (const radio of document.querySelectorAll('input[name=mode]')) radio.onchange=()=>{ $('since').disabled=document.querySelector('input[name=mode]:checked').value!=='since'; };
$('auth').onclick=async()=>{
  let client_json;
  try { const file=$('client').files[0]; if (file) { if (file.size>24000) throw new Error(); client_json=JSON.parse(await file.text()); } }
  catch { $('notice').textContent='Desktop appのJSONファイルを確認してください。'; return; }
  $('client').value='';
  await act({op:'auth',gmail:$('gmail').value.trim(),client_json});
};
$('preview').onclick=()=>act({op:'preview'});
$('send').onclick=()=>act({op:'send'});
await refresh();
let pollActive=false;
setInterval(async()=>{ if (pollActive || submitting) return; pollActive=true; try { await refresh(); } finally { pollActive=false; } },2000);
