/* Saved, capture-anchored Git evidence. Loading a report never queries Git. */
"use strict";
const HeaderHotspots = (() => {
  const filters = {days:90,q:"",target:"",offset:0};
  let request = 0;
  let detailRequest = 0;
  let currentRun = null;
  function shell() {
    const targets=[...new Set(state.run.project.units.map(targetFor))].sort();
    if(currentRun!==state.run.id){filters.offset=0;currentRun=state.run.id;}
    if(filters.target && !targets.includes(filters.target))filters.target='';
    return '<p class="eyebrow">FREQUENTLY EDITED EXPENSIVE HEADERS</p><h2>Header hotspots</h2>'+
      '<p>Prioritize headers by committed edits × associated compilation work in this capture. This is a prioritization signal, not predicted savings.</p>'+
      '<div class="toolbar"><input id="hotspot-filter" aria-label="Filter header hotspots" placeholder="Filter headers" value="'+esc(filters.q)+'">'+
      '<select id="hotspot-target" aria-label="Hotspot target"><option value="">All targets</option>'+targets.map(t=>'<option value="'+esc(t)+'"'+(t===filters.target?' selected':'')+'>'+esc(t)+'</option>').join('')+'</select>'+
      '<select id="hotspot-days" aria-label="History window">'+[30,90,180].map(d=>'<option value="'+d+'"'+(d===filters.days?' selected':'')+'>'+d+' days</option>').join('')+'</select></div><div id="header-hotspots-content" aria-live="polite"></div>';
  }
  async function load() {
    const token=++request,run=state.run.id;
    const node=$("#header-hotspots-content");
    if(!node)return;
    if(typeof BuildscopeTour!=="undefined"&&BuildscopeTour.active){node.innerHTML='<div class="empty">The guided example has no saved committed-header history. Select a recorded capture to analyze its history.</div>';return;}
    node.innerHTML='<p class="muted">Loading saved history…</p>';
    try {
      const data=await api('/api/header-hotspots',{run, ...filters,limit:100});
      if(token!==request || state.run.id!==run || !node.isConnected)return;
      if(!data.available){node.innerHTML=notes(data.notes||[])+ '<div class="empty">Analyze history explicitly: <code>buildscope analyze-history '+esc(run)+' --days 90</code>, then reload this view.</div>';return;}
      const messages=[...(data.notes||[])];
      if(data.window_truncated)messages.push('The saved analysis covers less than the selected window. Edit counts are lower bounds and priority is unavailable. Analyze a longer window to complete it.');
      if(!data.complete)messages.push('History coverage is incomplete. Counts show known committed edits; priority requires complete history and a successful managed profile with all dependent units measured.');
      node.innerHTML='<p class="small muted">Anchor '+esc(data.anchor)+' · '+esc(date(data.since_ms))+' – '+esc(date(data.until_ms))+'</p>'+notes(messages)+
        table([{label:'Header',render:r=>esc(r.key)},{label:'Edits',numeric:true,render:r=>num(r.editing_commits)},
          {label:'Last edited',render:r=>r.last_edited_ms==null?'No edits':esc(date(r.last_edited_ms))},
          {label:'Dependent units',numeric:true,render:r=>num(r.dependent_units)},
          {label:'Known compilation work',numeric:true,render:r=>time(r.work_ms)+'<div class="small muted">'+num(r.measured_units)+' / '+num(r.dependent_units)+' units</div>'},
          {label:'Recorded processing',numeric:true,render:r=>time(r.processing_ms)},
          {label:'Priority (edit × ms)',numeric:true,render:r=>r.priority==null?'Unavailable':num(Math.round(r.priority))}],data.rows||[],{action:'header-hotspot',id:r=>r.path})+
        '<div class="pagination"><button id="hotspot-prev"'+(filters.offset===0?' disabled':'')+'>← Previous</button><span>'+num(filters.offset+((data.rows||[]).length?1:0))+'–'+num(filters.offset+(data.rows||[]).length)+(data.total!=null?' of '+num(data.total):'')+'</span><button id="hotspot-next"'+((data.rows||[]).length<100 || (data.total!=null && filters.offset+100>=data.total)?' disabled':'')+'>Next →</button></div>';
      $('#hotspot-prev').onclick=()=>{filters.offset=Math.max(0,filters.offset-100);load();};
      $('#hotspot-next').onclick=()=>{filters.offset+=100;load();};
    } catch(error){if(token===request&&node.isConnected)node.innerHTML=notes([error.message]);}
  }
  function bind() {
    for(const [id,key] of [['hotspot-filter','q'],['hotspot-target','target'],['hotspot-days','days']]){
      const control=$('#'+id);if(!control)continue;
      control[key==='q'?'oninput':'onchange']=()=>{filters[key]=key==='days'?Number(control.value):control.value;filters.offset=0;load();};
    }
    load();
  }
  async function detail(path) {
    const run=state.run.id, token=++detailRequest;
    state.detail={type:'header-hotspot',id:path};
    try {
      const data=await api('/api/header-hotspots',{run,days:filters.days,target:filters.target,path,limit:1});
      if(state.run.id!==run || token!==detailRequest || state.detail?.type!=='header-hotspot' || state.detail.id!==path)return;
      const row=data.rows?.[0];
      if(!row){modal(notes(data.notes||['No saved history for this header.']));return;}
      modal('<p class="eyebrow">HEADER HOTSPOT EVIDENCE</p><h2>'+esc(row.key)+'</h2><p class="muted">Saved capture anchor '+esc(data.anchor)+'</p>'+
        '<div class="metrics">'+metric('Committed edits',num(row.editing_commits),filters.days+' day requested window')+
        metric('Known compilation work',time(row.work_ms),num(row.measured_units)+' / '+num(row.dependent_units)+' dependent units measured')+
        metric('Priority',row.priority==null?'Unavailable':num(Math.round(row.priority)),'Edit count × compilation milliseconds')+'</div>'+notes(data.notes||[])+
        '<button class="action" data-action="header" data-id="'+esc(row.path||path)+'">Open header dependencies and includes</button><h3>Content-changing commits</h3>'+((row.edits||[]).length?'<ul>'+row.edits.map(e=>'<li><code>'+esc(e.commit)+'</code> · '+esc(date(e.committed_ms))+'</li>').join('')+'</ul>':'<p>No content-changing commits recorded in this window.</p>')+
        '<h3>Detected renames</h3>'+((row.renames||[]).length?'<ul>'+row.renames.map(r=>'<li>'+esc(r[0])+' → '+esc(r[1])+'</li>').join('')+'</ul>':'<p>No renames detected in the saved window.</p>'));
    } catch(error){toast(error.message);}
  }
  return {shell,load,bind,detail};
})();
