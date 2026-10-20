/* Saved boundary comparisons; no file reads or savings prediction in the report. */
"use strict";
const RebuildDetector = (() => {
  let serial = 0;
  function shell() { return '<div id="rebuild-detector" class="panel"><p class="muted">Loading rebuild evidence…</p></div>'; }
  function stamp(value) { return value ? 'SHA256 '+(value.digest||'unknown')+' · '+value.size+' bytes · mtime '+value.modified_ns : 'Missing / unobserved'; }
  function references(ids) {
    return ids.map(id=>{
      const action=typeof state!=="undefined"?state.run.actions.find(a=>a.id===id):null;
      return esc(id)+(action?.unit_id?' <button class="action small" data-action="unit" data-id="'+esc(action.unit_id)+'">Inspect unit</button>':'');
    }).join(', ');
  }
  function render(report) {
    if (!report || report.available === false) return '<div class="empty">Rebuild evidence unavailable for this older capture or collection mode.</div>';
    const candidates=report.candidates||[];
    return '<h2>Unnecessary rebuild candidates</h2><p>'+candidates.length+(candidates.length===1?' candidate · ':' candidates · ')+esc(time(report.combined_work_ms))+' combined associated action work (union). No elapsed-time savings estimate.</p>'+notes(report.notes||[])+
      '<p>Previous local compatible capture: '+esc(report.previous||'Unavailable')+' · '+(report.eligible?'Successful ordinary recording':'Conclusions unavailable')+'</p>'+artifactLink('rebuild-evidence.json')+
      candidates.map(c=>'<section class="panel"><h3>'+esc(c.path)+'</h3><p>'+esc(c.category)+' · '+esc(c.support)+' · '+esc(time(c.work_ms))+' overlapping associated work</p><p>Dependent actions: '+references(c.actions)+'</p><p>Generator actions: '+(references(c.generators)||'None observed')+'</p><p>Fingerprint comparison '+esc(c.comparison)+' below</p>'+notes(c.reasons||[])+'</section>').join('')+
      '<h3>All recorded boundary changes</h3><p>Other content and timestamp changes may also require rebuilding. Unknown hashes do not establish unchanged contents.</p>'+table([
        {label:'Comparison',render:c=>esc((report.comparisons||[]).indexOf(c))}, {label:'Input',render:c=>esc(c.path)}, {label:'Interval',render:c=>esc(c.interval)},
        {label:'Before',render:c=>esc(stamp(c.before))}, {label:'After',render:c=>esc(stamp(c.after))}
      ],report.comparisons||[]);
  }
  async function load() {
    const current=++serial, run=state.run.id;
    if(typeof BuildscopeTour!=="undefined"&&BuildscopeTour.active){$("#rebuild-detector").innerHTML='<div class="empty">The guided example has no saved fingerprint boundaries. Select an ordinary recording to inspect rebuild candidates.</div>';return;}
    try { const report=await api('/api/rebuilds',{run}); if(current===serial&&state.run.id===run&&$('#rebuild-detector'))$('#rebuild-detector').innerHTML=render(report); }
    catch(error) { if(current===serial&&$('#rebuild-detector'))$('#rebuild-detector').innerHTML=notes([error.message]); }
  }
  return {shell,load,render,stamp};
})();
if(typeof module!=="undefined")module.exports=RebuildDetector;
