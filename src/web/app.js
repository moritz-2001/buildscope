/* Buildscope local report. Facts retain coverage; raw compiler events are queried in pages. */
"use strict";
const $ = (selector, root = document) => root.querySelector(selector);
const esc = value => String(value ?? "").replace(/[&<>"']/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const num = value => new Intl.NumberFormat().format(value ?? 0);
const time = value => value == null ? "Unavailable" : value >= 1000 ? (value / 1000).toFixed(2) + " s" : value.toFixed(1) + " ms";
const signed = value => value == null ? "Unavailable" : (value > 0 ? "+" : "") + time(value);
const bytes = value => value >= 1048576 ? (value / 1048576).toFixed(1) + " MiB" : num(value) + " B";
const date = value => new Date(value).toLocaleString();
const state = {run:null,runs:[],view:"overview",q:"",scope:"all",target:"all",sort:"processing_ms",descending:true,detail:null};
let generation = 0;
const profileByUnit = new Map();
const unitById = new Map();
const actionWork = new Map();
const detailLoaded = new Set();
const tableBindings = new WeakMap();
let nextTable = 0;

async function api(path, query = {}) {
  if (BuildscopeTour.active) return BuildscopeTour.request(path, query);
  const response = await fetch(path + "?" + new URLSearchParams(query));
  const value = await response.json();
  if (!response.ok) throw new Error(value.error || response.statusText);
  return value;
}
function toast(message) {
  $("#toast").textContent = message; $("#toast").style.display = "block";
  clearTimeout(toast.timer); toast.timer = setTimeout(() => $("#toast").style.display = "none", 5500);
}
function metric(label, value, caption) {
  return '<div class="metric"><div class="muted small">' + esc(label) + '</div><div class="number">' + esc(value) +
    '</div><div class="caption">' + esc(caption) + '</div></div>';
}
function notes(items) { return items.length ? '<div class="notes">' + items.map(n => '<p>' + esc(n) + '</p>').join("") + '</div>' : ""; }
function pill(text, warning = false) { return '<span class="pill' + (warning ? " warning" : "") + '">' + esc(text) + '</span>'; }
function pathKey(path) {
  if (path.startsWith(state.run.project.build_dir + "/")) return "<build>/" + path.slice(state.run.project.build_dir.length + 1);
  if (path.startsWith(state.run.project.source_dir + "/")) return path.slice(state.run.project.source_dir.length + 1);
  return path;
}
function targetFor(unit) { return unit.output_key.match(/CMakeFiles\/(.+?)\.dir\//)?.[1] || "(other)"; }
function sourceButton(path, line) {
  return '<button class="action small" data-action="source" data-path="' + esc(path) + '" data-line="' + esc(line || "") + '">View source' + (line ? ":" + line : "") + '</button>';
}
function artifactLink(path) {
  if (BuildscopeTour.active) return '<span class="small muted">Example evidence · no artifact download</span>';
  return '<a class="small" href="/api/artifact?' + esc(new URLSearchParams({run:state.run.id,path})) + '">Download raw evidence ↗</a>';
}
function table(columns, rows, rowAction, page=0) {
  if (!rows.length) return '<div class="empty">No matching observations in this capture.</div>';
  const pageSize=100, start=page*pageSize, visible=rows.slice(start,start+pageSize),id="table-"+(++nextTable);
  const binding={columns,rows,rowAction,page};
  queueMicrotask(()=>{const node=document.querySelector('[data-table="'+id+'"]');if(node)tableBindings.set(node,binding);});
  return '<div data-table="'+id+'"><div class="table-wrap"><table><thead><tr>' + columns.map(c =>
    '<th class="' + (c.numeric ? "numeric " : "") + (c.key ? "sortable" : "") + '"' +
    (c.key ? ' tabindex="0" role="button" data-sort="' + esc(c.key) + '"' : "") +
    (c.tooltip ? ' title="' + esc(c.tooltip) + '" aria-description="' + esc(c.tooltip) + '"' : "") + '>' + esc(c.label) +
    (state.sort === c.key ? state.descending ? " ↓" : " ↑" : "") + '</th>').join("") +
    '</tr></thead><tbody>' + visible.map(row => '<tr' + (rowAction ? ' class="clickable" tabindex="0" data-action="' + rowAction.action +
      '" data-id="' + esc(rowAction.id(row)) + '"' : "") + '>' + columns.map(c =>
      '<td class="' + (c.numeric ? "numeric" : "") + '">' + c.render(row) + '</td>').join("") + '</tr>').join("") + '</tbody></table></div>'+
      (rows.length>pageSize?'<div class="pagination"><button data-action="table-page" data-id="-1"'+(page===0?' disabled':"")+'>← Previous</button><span>'+
      num(start+1)+"–"+num(Math.min(start+pageSize,rows.length))+" of "+num(rows.length)+'</span><button data-action="table-page" data-id="1"'+
      (start+pageSize>=rows.length?' disabled':"")+'>Next →</button></div>':"")+'</div>';
}

function sortRows(rows) {
  return rows.sort((a,b) => {
    const x=a[state.sort], y=b[state.sort];
    const order = typeof x === "string" && typeof y === "string" ? x.localeCompare(y) : (x ?? -Infinity) - (y ?? -Infinity);
    return state.descending ? -order : order;
  });
}
function toolbar(placeholder) {
  const targets=[...new Set(state.run.project.units.map(targetFor))].sort();
  return '<div class="toolbar"><input id="filter" aria-label="Filter report" placeholder="' + esc(placeholder) + '" value="' + esc(state.q) +
    '"><select id="target-filter" aria-label="Target filter"><option value="all">All targets</option>' +
    targets.map(t => '<option' + (state.target===t?" selected":"") + '>' + esc(t) + '</option>').join("") + '</select>' +
    '<select id="scope-filter" aria-label="Path filter">' + [["all","All paths"],["project","Project sources"],["generated","Generated files"],["external","External / system"]]
      .map(([v,label]) => '<option value="' + v + '"' + (state.scope===v?" selected":"") + '>' + label + '</option>').join("") + '</select></div>';
}
function inScope(key) {
  return state.scope==="all" || (state.scope==="generated" ? key.startsWith("<build>") :
    state.scope==="external" ? key.startsWith("<external>") || key.startsWith("/") : !key.startsWith("<"));
}
function selectedUnits() { return state.run.project.units.filter(u => state.target==="all" || targetFor(u)===state.target); }
function headerRows() {
  const selected = new Set(selectedUnits().map(u=>u.id));
  return state.run.headers.filter(h => inScope(h.key) && h.key.toLowerCase().includes(state.q.toLowerCase()))
    .map(h => {
      const units=h.units.filter(id=>selected.has(id));
      return {...h,units,reach:units.length,source_count:new Set(units.map(id=>unitById.get(id)?.source)).size,
        trigger_count:h.history?.record?.trigger_runs??null,direct:h.direct_units.filter(id=>selected.has(id)).length,
        processing_ms:h.processing_ms,rebuilt:units.filter(id=>actionWork.has(id)).length,
        work:units.reduce((sum,id)=>sum+(actionWork.get(id)||0),0)};
    }).filter(h=>h.reach>0);
}
function unitRows() {
  return selectedUnits().filter(u=>inScope(u.source_key) && (u.source_key+" "+u.id).toLowerCase().includes(state.q.toLowerCase())).map(u=>{
    const p=profileByUnit.get(u.id);
    const phase=name=>p?.phases.find(m=>name.includes(m.name) && m.unit==="ms")?.value??null;
    return {...u,family:state.run.project.compilers[u.compiler]?.family||"unknown",work:actionWork.get(u.id)??null,
      frontend:phase(["Frontend","phase parsing"]),backend:phase(["Backend","phase opt and generate"]),
      templates:phase(["InstantiateFunction","template instantiation"])};
  });
}
function headerColumns() {
  return [
    {label:"Header",key:"key",
      tooltip:"Header path in the dependency inventory or compiler observations. Click a row to inspect its include paths and dependent units. Supported rebuild trigger means Ninja explicitly named this header as a dirty input and dependents rebuilt; Changed contributor means the header changed and dependents rebuilt, without explicit trigger evidence.",
      render:h=>'<span class="path">' + esc(h.key) + '</span><div class="small muted">' +
      (h.explicit_trigger?"Supported rebuild trigger":h.changed_contributor?"Changed contributor":"") + '</div>'},
    {label:"Source files / units",key:"reach",numeric:true,
      tooltip:"Distinct source files / compilation units that depend on this header, directly or indirectly, within the selected target. A unit is a source/output pair, so one source compiled into multiple targets can count as multiple units. Includes units that did not rebuild; dependency records may come from earlier compilations.",
      render:h=>num(h.source_count)+" / "+num(h.reach)},
    {label:"Direct · recorded",key:"direct",numeric:true,
      tooltip:"Number of distinct compilation units in the selected target where compiler observations recorded this header as directly included by the source (include depth 1). A zero does not prove there are no direct includes: include observations may be incomplete or unavailable.",
      render:h=>num(h.direct)},
    {label:"Processing · capture total",key:"processing_ms",numeric:true,
      tooltip:"Sum of measured inclusive processing time for this header across profiled units in the entire capture. Includes time in nested headers, so header totals can overlap. This value does not change with the target filter. Unavailable means no timing was measured; it does not mean zero cost. This is not an estimate of time saved by removing the header.",
      render:h=>time(h.processing_ms)},
    {label:"Units rebuilt",key:"rebuilt",numeric:true,
      tooltip:"Number of distinct dependent compilation units in the selected target with a recorded compile action in this capture. Counts each unit once, even if it has multiple actions. Rebuilding does not by itself establish that this header caused the rebuild.",
      render:h=>num(h.rebuilt)},
    {label:"Associated compile work",key:"work",numeric:true,
      tooltip:"Sum of full compile action durations for rebuilt units that depend on this header, directly or indirectly, in this capture and selected target. A unit's full duration is attributed to every header it depends on, so header rows overlap and should not be added together. Parallel work can exceed elapsed build time. This is not the header's processing time or an estimate of time saved by removing it.",
      render:h=>time(h.work)},
    {label:"Native trigger · ordinary builds",key:"trigger_count",numeric:true,
      tooltip:"Ordinary recorded builds where Ninja explicitly identified this header as a dirty input and dependents rebuilt / ordinary recorded builds containing this header. History uses the same build configuration and recorded target set, excludes instrumented profiles, and does not change with the UI target filter. No history means no compatible ordinary recordings are available.",
      render:h=>h.history?.record?
      num(h.history.record.trigger_runs)+" / "+num(h.history.record.captures):"No history"}
  ];
}
function unitColumns() {
  return [
    {label:"Compilation unit",key:"source_key",render:u=>'<span class="path">' + esc(u.source_key) + '</span><div class="small muted">' + esc(u.id) + ' · ' + esc(targetFor(u)) + '</div>'},
    {label:"Compiler",key:"family",render:u=>pill(u.family)},
    {label:"Action duration",key:"work",numeric:true,render:u=>time(u.work)},
    {label:"Frontend",key:"frontend",numeric:true,render:u=>time(u.frontend)},
    {label:"Backend",key:"backend",numeric:true,render:u=>time(u.backend)},
    {label:"Template activity",key:"templates",numeric:true,render:u=>time(u.templates)}
  ];
}
function findingsHTML(findings) {
  if(!findings.length) return '<div class="empty">No findings supported by the available measurements.</div>';
  return findings.map((f,i)=>'<div class="finding">' + pill(f.origin) +
    '<button data-action="finding" data-id="' + state.run.findings.indexOf(f) + '">' + esc(f.title) + '</button><p>' + esc(f.explanation) +
    '</p><details><summary class="small muted">Evidence and next experiment</summary><pre class="evidence">'+esc(f.evidence.join("\n"))+
    '</pre><p>'+esc(f.experiment)+'</p></details></div>').join("");
}
function render() {
  if(!state.run)return;
  const run=state.run, launcher=run.kind==="launcher";
  const titles={overview:"Understand the wait.",headers:"Which headers create work?",units:"Why is this file expensive?",history:"Your build history.",compare:"Measure what changed."};
  $("#title").textContent=titles[state.view];
  $("#breadcrumb").textContent=state.view==="units"?"COMPILATION UNITS":state.view.toUpperCase();
  $("#status").textContent=(BuildscopeTour.active?"Example data · ":"")+run.status+" · "+run.kind; $("#status").className="badge "+run.status;
  $("#header-count").textContent=num(run.headers.length);
  $("#context").textContent=(run.label?run.label+" · ":"")+date(run.started_ms)+" · "+run.project.build_type+" · "+gitLabel(run.git)+" · "+run.project.build_dir;
  document.querySelectorAll("nav button").forEach(b=>{b.disabled=false;b.classList.toggle("active",b.dataset.view===state.view);});
  $("#run").disabled=false;
  let html="";
  if(state.view==="overview") {
    html='<div class="metrics">' + metric(launcher?"Observed compiler span":"Elapsed build time",time(run.elapsed_ms),launcher?"First to last observed compiler; excludes link/generators":"Ninja execution; excludes postprocessing") +
      metric("Accumulated action work",time(run.summary.work_ms),"Concurrent work can exceed elapsed time") +
      metric("Observed compilations",num(run.summary.observed_units)+" / "+num(run.project.units.length),"Executed units / compilation inventory") +
      metric("Peak concurrency",num(run.summary.max_concurrency),"Simultaneous actions · not CPU usage") + '</div>' +
      '<div class="panel"><div class="panel-head"><h2>What happened through time</h2><span class="small muted">' + num(run.actions.length) + ' executed actions</span></div>' +
      '<canvas id="build-timeline" class="timeline" aria-label="Build action timeline"></canvas><div id="timeline-label" class="timeline-label">Hover an action for evidence; click a compilation to investigate.</div>' +
      '<div class="legend"><span><i class="compile"></i>Compile</span><span>Link / generate / PCH / module shown in separate colors</span></div>' +
      notes([launcher?"Compiler-only timeline. Build boundaries and the full build dependency chain were not observed.":"Dependency-chain work: "+time(run.summary.dependency_path_ms)+". This ignores resource constraints and does not predict elapsed-time savings."]) + '</div>' +
      '<div class="grid"><div class="panel"><h2>Evidence-backed findings</h2>' + findingsHTML(run.findings) + '</div>' +
      '<div><div class="panel"><h2>Capture coverage</h2><p>' + run.project.compilers.map(c=>pill(c.family)+" "+esc(c.version.split("\n")[0])).join("</p><p>") +
      '</p><p class="small muted">Dependency records: '+num(run.summary.dependency_units)+" / "+num(run.project.units.length)+" units.</p>" +
      '<p class="small muted">Compiler detail: '+num(run.profiles.length)+' invocations. Cache policy: '+esc(run.cache_policy)+'.</p>' +
      '<p class="small">Git: '+gitDetails(run.git)+'</p>'+notes(["available","mixed"].includes(run.git?.status)?["Git state was observed before execution. Uncommitted changes and edits during compilation mean HEAD alone does not identify the compiled contents."]:[])+
      notes(run.notes)+'</div><div class="panel"><h2>Start an investigation</h2><p class="muted small">Follow a costly header into its dependent units, then inspect compiler activity and retained evidence.</p>' +
      '<button class="action" data-action="view" data-id="headers">Explore headers →</button></div></div></div>';
  } else if(state.view==="headers") {
    html='<p class="muted">Reach, processing cost, and rebuild impact describe different kinds of work.</p>'+toolbar("Filter headers by path…")+
      notes(["Processing totals cover the entire capture; target selection narrows reach and rebuilt work. Inclusive costs overlap. Reach comes from the available dependency inventory; direct counts describe recorded includes. Historical triggers count matching configurations and requested target sets; ordinary builds are separate from profiles."])+
      table(headerColumns(),sortRows(headerRows()),{action:"header",id:h=>h.path});
  } else if(state.view==="units") {
    html='<p class="muted">Inspect the compiler activity behind an expensive compilation.</p>'+toolbar("Filter source files or unit IDs…")+
      notes(["Unexecuted units have no action timing in this run. Compiler phase/subphase measurements may overlap."])+
      table(unitColumns(),sortRows(unitRows()),{action:"unit",id:u=>u.id});
  } else if(state.view==="history") {
    html='<p class="muted">Ordinary recordings, managed profiles, and collected launcher batches have different timing coverage.</p><div class="panel"><canvas id="history-chart" class="spark" aria-label="Build history"></canvas></div>'+
      table([
        {label:"Capture",render:r=>'<span class="path">'+esc(r.id)+'</span><div class="muted small">'+esc(r.label||"")+'</div>'},
        {label:"When",render:r=>esc(date(r.started_ms))},{label:"Mode",render:r=>pill(r.kind)},
        {label:"Git",render:r=>gitDetails(r.git)},
        {label:"Status",render:r=>esc(r.status)},{label:"Observed duration",numeric:true,render:r=>time(r.elapsed_ms)},
        {label:"Actions",numeric:true,render:r=>num(r.actions)}
      ],state.runs,{action:"run",id:r=>r.id});
  } else if(state.view==="compare") {
    html='<p class="muted">Match entities and check measurement conditions before interpreting a difference.</p>'+
      '<div class="toolbar"><select id="comparison-mode" aria-label="Comparison mode"><option value="captures">Individual captures</option><option value="series">Repeated measurements by label</option></select></div><div class="comparison-selects">'+
      '<select id="before-run" aria-label="Before capture">'+runOptions(state.runs.find(r=>r.id!==run.id)?.id||run.id)+'</select>'+
      '<select id="after-run" aria-label="After capture">'+runOptions(run.id)+'</select><button class="action" id="compare-button">Compare</button></div><div id="comparison"></div>';
  }
  $("#content").innerHTML=html;
  bindFilters();
  if(state.view==="overview") drawBuildTimeline();
  if(state.view==="history") drawHistory();
  if(state.view==="compare") {
    $("#compare-button").onclick=()=>showComparison().catch(e=>toast(e.message));
    $("#comparison-mode").onchange=()=>{
      const labels=[...new Set(state.runs.filter(r=>r.label&&r.status==="success").map(r=>r.label))];
      if($("#comparison-mode").value==="series") {
        const beforeLabel=state.runs.find(r=>r.id===$("#before-run").value)?.label;
        const afterLabel=state.runs.find(r=>r.id===$("#after-run").value)?.label;
        const options=labels.map(label=>'<option>'+esc(label)+'</option>').join("");
        $("#before-run").innerHTML=options;$("#after-run").innerHTML=options;
        if(labels.length>1)$("#after-run").selectedIndex=1;
        if(labels.includes(beforeLabel))$("#before-run").value=beforeLabel;
        if(labels.includes(afterLabel))$("#after-run").value=afterLabel;
      }else{$("#before-run").innerHTML=runOptions(state.runs.find(r=>r.id!==state.run.id)?.id);$("#after-run").innerHTML=runOptions(state.run.id);}
      showComparison().catch(e=>toast(e.message));
    };
    showComparison().catch(e=>toast(e.message));
  }
}
function gitLabel(git) {
  let label;
  switch(git?.status) {
    case "available": label=(git.branch??"Detached HEAD")+" · "+(git.commit?.slice(0,12)??"no commits yet");break;
    case "mixed": label="Multiple Git revisions or unavailable observations";break;
    case "not_repository": label="No Git repository";break;
    case "unavailable": label="Git information unavailable";break;
    default: label="Git information not recorded";
  }
  return label+(git?.dirty===true?" · uncommitted changes":"");
}
function gitDetails(git) {
  return '<span title="'+esc(git?.commit??gitLabel(git))+'">'+esc(gitLabel(git))+'</span>'+
    (git?.commit?' <button class="action small" data-action="copy-commit" data-id="'+esc(git.commit)+'">Copy commit</button>':"");
}
function runOptions(selected) {
  return state.runs.map(r=>'<option value="'+esc(r.id)+'"'+(r.id===selected?" selected":"")+'>'+esc((r.label||r.id.slice(-14))+" · "+r.kind+" · "+gitLabel(r.git)+" · "+date(r.started_ms))+'</option>').join("");
}
function bindFilters() {
  const input=$("#filter");
  if(input) input.oninput=()=>{
    const position=input.selectionStart;state.q=input.value;render();
    $("#filter").focus();$("#filter").setSelectionRange(position,position);
  };
  if($("#scope-filter")) $("#scope-filter").onchange=e=>{state.scope=e.target.value;render();};
  if($("#target-filter")) $("#target-filter").onchange=e=>{state.target=e.target.value;render();};
}
async function loadRun(id) {
  const version=++generation;
  const run=await api("/api/run",{id});
  if(version!==generation)return;
  state.run=run;state.q="";state.target="all";state.scope="all";
  indexRun(run);
  $("#run").value=run.id;
  history.replaceState(null,"",BuildscopeTour.active?"?tour=1#"+state.view:"?run="+encodeURIComponent(run.id)+"#"+state.view);
  render();
}
function indexRun(run) {
  profileByUnit.clear();unitById.clear();actionWork.clear();detailLoaded.clear();
  run.profiles.forEach(p=>profileByUnit.set(p.unit_id,p));
  run.project.units.forEach(u=>unitById.set(u.id,u));
  run.actions.forEach(a=>{if(a.unit_id)actionWork.set(a.unit_id,(actionWork.get(a.unit_id)||0)+a.end_ms-a.start_ms);});
}
function modal(html) {
  $("#detail-body").innerHTML=html;
  if(!$("#detail").open)$("#detail").showModal();
  BuildscopeTour.mount();
}
async function openHeader(path) {
  const runId=state.run.id;
  const header=await api("/api/header",{run:state.run.id,path});
  if(!header||state.run.id!==runId)return;
  state.detail={type:"header",id:path};
  const samples=header.samples_ms;
  const middle=Math.floor(samples.length/2);
  const median=samples.length?(samples.length%2?samples[middle]:(samples[middle-1]+samples[middle])/2):null;
  modal('<p class="eyebrow">HEADER INVESTIGATION</p><h2 class="detail-title">'+esc(header.key)+'</h2><p class="detail-meta">'+esc(header.path)+'</p>'+
    '<div class="metrics">'+metric("Dependent units",num(header.units.length),"Available dependency inventory")+
    metric("Inclusive processing",time(header.processing_ms),num(header.measured_units)+" measured units")+
    metric("Median per measured unit",time(median),"Range "+time(samples[0])+" – "+time(samples.at(-1)))+
    metric("Associated rebuilt work",time(header.rebuilt_work_ms),num(header.rebuilt_units)+" rebuilt units · shared attribution")+'</div>'+
    sourceButton(header.path)+notes(["Recorded-child-exclusive header work: "+time(header.recorded_self_ms)+". Filtered traces can omit child intervals; this is not an exact intrinsic header cost."])+
    '<h3>Why is this included?</h3><div id="include-paths"></div><h3>Dependent compilation units</h3>'+
    table(unitColumns().filter(c=>!c.key||["source_key","work","family"].includes(c.key)),
      header.units.map(id=>unitRowsForDetail(id)),{action:"unit",id:u=>u.id})+
    '<h3>Rebuild evidence</h3>'+notes(header.evidence.length?header.evidence:["No explicit Ninja invalidation explanation names this header in this capture."])+
    (header.changed_contributor?'<p class="small">This header changed and dependent units rebuilt. Other changes can share the same rebuilding work.</p>':"")+
    '<h3>Observed history</h3><div id="header-history" class="muted">Loading compatible captures…</div>');
  const paths=await api("/api/include-paths",{run:state.run.id,path});
  if(state.detail?.id!==path)return;
  $("#include-paths").innerHTML=paths.paths.length?paths.paths.map(row=>
    '<p class="bread">'+pill(row.units+" units")+" "+esc(row.chain.map(pathKey).join(" → "))+'</p>').join("")+
    (paths.total>200?notes(["Showing the 200 most common paths of "+num(paths.total)+"."]):""):
    '<p class="muted small">No include nesting was recorded for this header. Profile the project; flat dependency lists do not establish include paths.</p>';
  try {
    const rows=await api("/api/header-history",{run:state.run.id,key:header.key});
    if(state.detail?.id!==path)return;
    $("#header-history").innerHTML=notes(["Supported triggers and changed contributors are separate; counts describe recorded builds, not an estimated edit frequency."])+
      table([{label:"When",render:r=>esc(date(r.started_ms))},{label:"Mode",render:r=>pill(r.kind)},
        {label:"Native trigger",render:r=>r.explicit_trigger?"Supported":"—"},
        {label:"Changed contributor",render:r=>r.changed_contributor?"Observed":"—"},
        {label:"Rebuilt units",numeric:true,render:r=>num(r.rebuilt_units)},
        {label:"Associated work",numeric:true,render:r=>time(r.rebuilt_work_ms)}],rows);
  }catch(e){if($("#header-history"))$("#header-history").textContent=e.message;}
}
function unitRowsForDetail(id) {
  const u=unitById.get(id);
  return {...u,family:state.run.project.compilers[u.compiler]?.family||"unknown",work:actionWork.get(id)??null};
}
function shellQuote(value) { return "'"+String(value).replace(/'/g,"'\\''")+"'"; }
function investigationCommand(unit) {
  return "buildscope --store "+shellQuote(state.store)+" profile --build-dir "+shellQuote(state.run.profile_origin||state.run.project.build_dir)+" --unit "+unit.id+" --detail full";
}
async function openUnit(id,tab="summary") {
  if(!unitById.has(id))return;
  if(!detailLoaded.has(id)) {
    const runId=state.run.id;
    const detail=await api("/api/unit",{run:runId,unit:id});
    if(state.run.id!==runId)return;
    unitById.set(id,detail.unit);
    if(detail.profile)profileByUnit.set(id,detail.profile);
    detailLoaded.add(id);
  }
  const unit=unitById.get(id);
  const profile=profileByUnit.get(id);
  state.detail={type:"unit",id,tab};
  const tabs=[["summary","Summary"],["templates","Templates"],["functions","Functions"],["evaluations","Constant evaluation"],["includes","Includes"],["trace","Trace events"],["command","Command"]];
  modal('<p class="eyebrow">COMPILATION INVESTIGATION · '+esc(state.run.project.compilers[unit.compiler]?.family)+'</p>'+
    '<h2 class="detail-title">'+esc(unit.source_key)+'</h2><p class="detail-meta">'+esc(unit.id)+" · "+esc(unit.output_key)+'</p>'+
    '<div class="toolbar">'+sourceButton(unit.source)+'<button class="action" data-action="copy-profile" data-id="'+esc(id)+'"'+(BuildscopeTour.active?' disabled title="Available for recorded captures"':"")+'>Copy full investigation command</button>'+
    (profile?artifactLink(profile.artifact):"")+'</div><div class="tabs">'+tabs.map(([key,label])=>
      '<button data-action="unit-tab" data-id="'+esc(id)+'" data-tab="'+key+'" class="'+(tab===key?"active":"")+'">'+label+'</button>').join("")+'</div><div id="unit-detail"></div>');
  const container=$("#unit-detail");
  if(tab==="command") {
    container.innerHTML='<h3>Original compilation arguments</h3><pre class="evidence">'+esc(unit.arguments.map(shellQuote).join(" "))+'</pre>'+
      '<p class="muted small">Working directory: '+esc(unit.directory)+'</p><h3>Compiler identity</h3><pre class="evidence">'+
      esc(state.run.project.compilers[unit.compiler]?.version)+'</pre>'+
      (unit.execution_command?'<h3>Native Ninja command</h3><pre class="evidence">'+esc(unit.execution_command)+'</pre>':"")+
      Object.entries(unit.response_files||{}).map(([path,text])=>'<h3>Response file · '+esc(pathKey(path))+'</h3><pre class="evidence">'+esc(text)+'</pre>').join("");
  } else if(!profile) {
    container.innerHTML='<div class="empty">This capture has no compiler detail for this unit. Its dependency reach and ordinary action timing remain available.</div>'+
      '<h3>Collect a focused profile</h3><pre class="evidence">'+esc(investigationCommand(unit))+'</pre>';
  } else if(tab==="summary") {
    container.innerHTML='<div class="metrics">'+metric("Action duration",time(actionWork.get(id)),"Includes compiler instrumentation when enabled")+
      metric("Trace events",num(profile.event_count),profile.family==="clang"?"Recorded compiler scopes":"GCC aggregate measurements")+
      metric("Template spellings",num(profile.templates.length),"Distinct names among recorded scopes")+
      metric("Timed headers",num(profile.header_costs.length),"Textual processing · reused artifacts differ")+'</div>'+
      notes(profile.notes)+'<h3>Native compiler measurements</h3>'+
      table([{label:"Activity",render:m=>esc(m.name)},{label:"Measurement",numeric:true,render:m=>m.unit==="ms"?time(m.value):m.unit==="bytes"?bytes(m.value):num(m.value)},
        {label:"Coverage",render:m=>esc(m.coverage)}],profile.phases)+
      '<h3>Related findings</h3>'+findingsHTML(state.run.findings.filter(f=>f.subject===id));
  } else if(["templates","functions","evaluations"].includes(tab)) {
    const entries=profile[tab];
    container.innerHTML=notes([profile.family==="gcc"?"GCC's standard report has no named specialization/function timings. Use its aggregate timers, or collect a separate Clang investigation.":
      "Inclusive recorded scopes can overlap. Counts are affected by trace granularity. Source links use supplied metadata."])+
      (tab==="templates"&&entries.length?'<div class="toolbar"><label><input type="checkbox" id="group-templates"> Group by approximate display family</label></div>':"")+
      '<div id="symbols"></div>';
    const renderSymbols=group=>{
      let symbols=entries;
      if(group) {
        const groups=new Map();
        for(const e of entries) {
          const key=family(e.name),value=groups.get(key)||{name:key,total_ms:0,count:0,max_ms:0};
          value.total_ms+=e.total_ms;value.count+=e.count;value.max_ms=Math.max(value.max_ms,e.max_ms);groups.set(key,value);
        }
        symbols=[...groups.values()].sort((a,b)=>b.total_ms-a.total_ms);
      }
      $("#symbols").innerHTML=table([
        {label:group?"Approximate family":"Compiler spelling",render:s=>'<span class="path">'+esc(s.name)+'</span>'},
        {label:"Inclusive work",numeric:true,render:s=>time(s.total_ms)},
        {label:"Recorded count",numeric:true,render:s=>num(s.count)},
        {label:"Longest",numeric:true,render:s=>time(s.max_ms)},
        {label:"Location",render:s=>s.file?sourceButton(s.file,s.line):'<span class="muted">Not supplied</span>'}
      ],symbols);
    };
    renderSymbols(false);
    if($("#group-templates"))$("#group-templates").onchange=e=>renderSymbols(e.target.checked);
  } else if(tab==="includes") {
    container.innerHTML=notes(["These edges are from the actual preprocessor include stack. Guarded/skipped include directives need not appear."])+
      table([{label:"Included by",render:e=>'<span class="path">'+esc(pathKey(e.parent))+'</span>'},
        {label:"Header",render:e=>'<button class="action small" data-action="header" data-id="'+esc(e.child)+'">'+esc(pathKey(e.child))+'</button>'},
        {label:"Depth",numeric:true,render:e=>num(e.depth)}],profile.includes);
  } else if(tab==="trace") {
    container.innerHTML=notes(["Events are local to this compiler invocation. Depth describes recorded time containment; filtered ancestors may be absent."])+
      '<div class="toolbar"><input id="event-query" placeholder="Filter symbols, headers, or passes…" aria-label="Filter events"><select id="event-kind" aria-label="Event activity"><option value="">All activities</option></select>'+
      '<select id="event-sort" aria-label="Event order"><option value="duration">Longest first</option><option value="time">Chronological</option></select></div>'+
      '<div id="trace-events"></div><div class="pagination"><button id="event-prev">← Previous</button><span id="event-range"></span><button id="event-next">Next →</button></div>';
    let offset=0,token=0;
    const load=async()=>{
      const current=++token;
      const result=await api("/api/events",{run:state.run.id,unit:id,q:$("#event-query").value,kind:$("#event-kind").value,
        sort:$("#event-sort").value,offset,limit:100});
      if(current!==token||state.detail?.id!==id||state.detail?.tab!=="trace")return;
      if($("#event-kind").options.length===1)for(const name of result.names)$("#event-kind").add(new Option(name,name));
      $("#trace-events").innerHTML=table([
        {label:"Activity",render:e=>'<span class="small">'+esc(e.name)+'</span><div class="muted small">depth '+e.depth+' · '+esc(e.thread)+'</div>'},
        {label:"Detail",render:e=>'<span class="path">'+esc(e.detail)+'</span>'},
        {label:"Start",numeric:true,render:e=>time(e.start_us/1000)},
        {label:"Duration",numeric:true,render:e=>time(e.duration_us/1000)},
        {label:"Location",render:e=>e.file?sourceButton(e.file,e.line):""}
      ],result.events,{action:"scope",id:e=>e.id});
      $("#event-range").textContent=num(result.total?offset+1:0)+"–"+num(Math.min(offset+100,result.total))+" of "+num(result.total);
      $("#event-prev").disabled=offset===0;$("#event-next").disabled=offset+100>=result.total;
    };
    $("#event-query").oninput=()=>{offset=0;load().catch(e=>toast(e.message));};
    $("#event-kind").onchange=$("#event-sort").onchange=()=>{offset=0;load().catch(e=>toast(e.message));};
    $("#event-prev").onclick=()=>{offset=Math.max(0,offset-100);load().catch(e=>toast(e.message));};
    $("#event-next").onclick=()=>{offset+=100;load().catch(e=>toast(e.message));};
    load().catch(e=>toast(e.message));
  }
}
async function openScope(index) {
  const id=state.detail?.id;
  if(!id||!unitById.has(id))return;
  const result=await api("/api/scope",{run:state.run.id,unit:id,id:index});
  const root=result.root;
  modal('<p class="eyebrow">RECORDED SCOPE · '+esc(root.name)+'</p><h2 class="detail-title">'+esc(root.detail||root.name)+'</h2>'+
    '<div class="toolbar"><button class="action" data-action="unit-tab" data-id="'+esc(id)+'" data-tab="trace">← Trace events</button>'+
    (root.parent!=null?'<button class="action" data-action="scope" data-id="'+root.parent+'">Containing scope ↑</button>':"")+
    (root.file?sourceButton(root.file,root.line):"")+'</div>'+
    '<p>'+time(root.duration_us/1000)+' · compiler-local start '+time(root.start_us/1000)+' · lane '+esc(root.thread)+'</p>'+
    notes(["Children are immediate recorded time-containment scopes. This is not a complete semantic template-instantiation dependency graph."])+
    '<h3>Recorded child scopes</h3>'+
    table([{label:"Activity",render:e=>esc(e.name)},{label:"Detail",render:e=>'<span class="path">'+esc(e.detail)+'</span>'},
      {label:"Duration",numeric:true,render:e=>time(e.duration_us/1000)}],result.children,{action:"scope",id:e=>e.id}));
}
function family(name) {
  let depth=0,result="";
  for(const c of name) {
    if(c==="<"){if(!depth)result+="<…>";depth++;}
    else if(c===">"&&depth)depth--;
    else if(!depth)result+=c;
  }
  return depth?name:result;
}
async function showSource(path,line) {
  const back=state.detail?{...state.detail}:null;
  const result=await api("/api/source",{run:state.run.id,path});
  modal('<p class="eyebrow">SOURCE EVIDENCE</p><h2 class="detail-title">'+esc(pathKey(path))+'</h2>'+
    (back?'<button id="source-back" class="action">← Back to investigation</button>':"")+
    notes([result.matches_capture?"Content matches this capture.":"Current content does not match the captured fingerprint. These lines are not evidence of the recorded source version."])+
    '<pre class="source">'+result.text.split("\n").map((text,i)=>'<span class="source-line'+(i+1===Number(line)?" selected":"")+'"><span class="lineno">'+(i+1)+'</span>'+esc(text)+'</span>').join("")+'</pre>');
  if(back)$("#source-back").onclick=()=>back.type==="unit"?openUnit(back.id,back.tab):openHeader(back.id).catch(e=>toast(e.message));
  $(".source-line.selected")?.scrollIntoView({block:"center"});
}
async function showComparison() {
  const before=$("#before-run").value,after=$("#after-run").value;
  if($("#comparison-mode")?.value==="series") {
    if(!before||!after){$("#comparison").innerHTML=notes(["Record successful captures with before/after experiment labels to compare repeated measurements."]);return;}
    const result=await api("/api/compare-series",{before_label:before,after_label:after});
    $("#comparison").innerHTML='<div class="metrics">'+metric("Before median",time(result.before.median_ms),num(result.before.count)+" successful samples · "+gitLabel(result.before.git))+
      metric("After median",time(result.after.median_ms),num(result.after.count)+" successful samples · "+gitLabel(result.after.git))+
      metric("Observed median difference",signed(result.delta_median_ms),"Observed duration; launcher batches exclude whole-build work")+
      metric("Recorded conditions",result.conditions_match?"Match":"Differ","Check configuration and cache conditions")+'</div>'+
      notes(result.limitations)+table([{label:"Label",render:r=>esc(r.label)},{label:"Samples",numeric:true,render:r=>num(r.count)},
      {label:"Median",numeric:true,render:r=>time(r.median_ms)},{label:"P10",numeric:true,render:r=>time(r.p10_ms)},
      {label:"P90",numeric:true,render:r=>time(r.p90_ms)},{label:"Minimum",numeric:true,render:r=>time(r.min_ms)},
      {label:"Maximum",numeric:true,render:r=>time(r.max_ms)}],[result.before,result.after]);
    return;
  }
  const result=await api("/api/compare",{before,after});
  if(!$("#comparison"))return;
  $("#comparison").innerHTML='<div class="metrics">'+metric("Before",time(result.before_elapsed_ms),result.before_timing_scope+" · "+gitLabel(result.before_git))+
    metric("After",time(result.after_elapsed_ms),result.after_timing_scope+" · "+gitLabel(result.after_git))+metric("Observed difference",signed(result.delta_elapsed_ms),"Repeat to establish a stable improvement")+
    metric("Conditions",result.conditions_match?"Match":"Differ","Recorded configuration and capture settings")+'</div>'+
    notes([...result.differences,...result.limitations])+'<h2>Matched compilation actions</h2>'+
    table([{label:"Source",render:u=>'<span class="path">'+esc(u.source)+'</span>'},
      {label:"Before",numeric:true,render:u=>time(u.before_ms)},{label:"After",numeric:true,render:u=>time(u.after_ms)},
      {label:"Difference",numeric:true,render:u=>'<span class="'+(u.delta_ms>0?"positive":"negative")+'">'+signed(u.delta_ms)+'</span>'}],result.units)+
    '<h2>Header changes</h2>'+table([{label:"Header",render:h=>'<span class="path">'+esc(h.key)+'</span>'},
      {label:"Dependent units",numeric:true,render:h=>h.before_units+" → "+h.after_units},
      {label:"Processing before",numeric:true,render:h=>time(h.before_processing_ms)},
      {label:"Processing after",numeric:true,render:h=>time(h.after_processing_ms)},
      {label:"Difference",numeric:true,render:h=>signed(h.delta_processing_ms)}],result.headers);
}
function drawBuildTimeline() {
  const canvas=$("#build-timeline");if(!canvas)return;
  const {ctx,width,height}=canvasContext(canvas);
  const actions=[...state.run.actions].sort((a,b)=>a.start_ms-b.start_ms);
  const end=actions.reduce((max,a)=>Math.max(max,a.end_ms),1);
  const lanes=[],rects=[];
  const colors={compile:"#8adbc0",link:"#91baf6",generate:"#edbb7c",configure:"#de9e8d",pch:"#c6a5e8",module:"#ef9ab3"};
  for(const action of actions){
    let lane=lanes.findIndex(t=>t<=action.start_ms);if(lane<0)lane=lanes.length;
    lanes[lane]=action.end_ms;rects.push({action,lane});
  }
  ctx.fillStyle="#9aa9b3";ctx.font="10px system-ui";
  for(let i=0;i<=5;i++){const x=width*i/5;ctx.fillText(time(end*i/5),Math.min(x,width-55),14);ctx.fillStyle="#293239";ctx.fillRect(x,24,1,height-26);ctx.fillStyle="#9aa9b3";}
  const row=Math.min(24,(height-35)/Math.max(lanes.length,1));
  for(const r of rects){r.x=r.action.start_ms/end*width;r.y=30+r.lane*row;r.w=Math.max(2,(r.action.end_ms-r.action.start_ms)/end*width);r.h=Math.max(2,row-4);
    ctx.fillStyle=colors[r.action.kind]||"#9aa9b3";ctx.fillRect(r.x,r.y,r.w,r.h);}
  const hit=event=>{const b=canvas.getBoundingClientRect();const x=(event.clientX-b.left)*width/b.width,y=(event.clientY-b.top)*height/b.height;return rects.find(r=>x>=r.x&&x<=r.x+r.w&&y>=r.y&&y<=r.y+r.h);};
  canvas.onmousemove=e=>{const r=hit(e);$("#timeline-label").textContent=r?r.action.kind+" · "+(unitById.get(r.action.unit_id)?.source_key||pathKey(r.action.outputs[0]||""))+" · "+time(r.action.end_ms-r.action.start_ms):"Hover an action for evidence; click a compilation to investigate.";};
  canvas.onclick=e=>{const r=hit(e);if(r?.action.unit_id)openUnit(r.action.unit_id).catch(e=>toast(e.message));};
}
function canvasContext(canvas) {
  const ratio=window.devicePixelRatio||1,width=canvas.clientWidth||600,height=canvas.clientHeight||200;
  canvas.width=width*ratio;canvas.height=height*ratio;
  const ctx=canvas.getContext("2d");ctx.scale(ratio,ratio);return {ctx,width,height};
}
function drawHistory() {
  const canvas=$("#history-chart");if(!canvas)return;
  const {ctx,width,height}=canvasContext(canvas);
  const rows=[...state.runs].filter(r=>r.configuration_id===state.run.project.configuration_id).reverse();
  const max=Math.max(...rows.map(r=>r.elapsed_ms),1);
  ctx.fillStyle="#9aa9b3";ctx.font="10px system-ui";ctx.fillText("Compatible configuration · points distinguish record / profile",0,12);
  for(let i=0;i<rows.length;i++){const x=15+(width-35)*i/Math.max(rows.length-1,1),y=height-10-(height-40)*rows[i].elapsed_ms/max;
    ctx.fillStyle=rows[i].kind==="record"?"#8adbc0":rows[i].kind==="launcher"?"#edbb7c":"#91baf6";ctx.beginPath();ctx.arc(x,y,4,0,Math.PI*2);ctx.fill();}
}
document.addEventListener("click",async event=>{
  const sortable=event.target.closest("[data-sort]");
  if(sortable){
    const key=sortable.dataset.sort;state.descending=state.sort===key?!state.descending:true;state.sort=key;
    const area=sortable.closest("[data-table]"),binding=tableBindings.get(area);
    if(sortable.closest("#detail")&&binding)area.outerHTML=table(binding.columns,sortRows([...binding.rows]),binding.rowAction);
    else render();
    return;
  }
  const button=event.target.closest("[data-action]");if(!button)return;
  const {action,id}=button.dataset;
  try{
    if(action==="table-page") {
      const area=button.closest("[data-table]"),binding=tableBindings.get(area);
      if(binding){area.outerHTML=table(binding.columns,binding.rows,binding.rowAction,binding.page+Number(id));}
    }
    else if(action==="header")await openHeader(id);
    else if(action==="unit")await openUnit(id);
    else if(action==="unit-tab")await openUnit(id,button.dataset.tab);
    else if(action==="scope")await openScope(Number(id));
    else if(action==="source")await showSource(button.dataset.path,button.dataset.line);
    else if(action==="copy-commit"){await navigator.clipboard.writeText(id);toast("Commit ID copied.");}
    else if(action==="copy-profile"){await navigator.clipboard.writeText(investigationCommand(unitById.get(id)));toast("Investigation command copied.");}
    else if(action==="run"){await loadRun(id);}
    else if(action==="view"){state.view=id;render();}
    else if(action==="finding"){
      const f=state.run.findings[Number(id)];
      if(unitById.has(f.subject))await openUnit(f.subject);
      else if(state.run.headers.some(h=>h.path===f.subject))await openHeader(f.subject);
      else modal('<h2>'+esc(f.title)+'</h2><p>'+esc(f.explanation)+'</p>'+notes(f.evidence));
    }
  }catch(error){toast(error.message);}
});
document.addEventListener("keydown",e=>{if((e.key==="Enter"||e.key===" ")&&e.target.matches("tr[data-action],th[data-sort]")){e.preventDefault();e.target.click();}});
document.querySelectorAll("nav button").forEach(button=>button.onclick=()=>{
  state.view=button.dataset.view;state.q="";state.sort=state.view==="units"?"work":"processing_ms";state.descending=true;
  history.replaceState(null,"",BuildscopeTour.active?"?tour=1#"+state.view:"?run="+encodeURIComponent(state.run.id)+"#"+state.view);render();window.scrollTo(0,0);
});
$("#close-detail").onclick=()=>{$("#detail").close();state.detail=null;};
$("#detail").addEventListener("cancel",()=>state.detail=null);
$(".brand").onclick=e=>{e.preventDefault();state.view="overview";render();window.scrollTo(0,0);};
$("#run").onchange=e=>loadRun(e.target.value).catch(error=>toast(error.message));
window.addEventListener("resize",()=>{if(state.view==="overview")drawBuildTimeline();else if(state.view==="history")drawHistory();});
function showWelcome() {
  $("#title").textContent="Explore Buildscope.";
  $("#breadcrumb").textContent="GETTING STARTED";
  $("#status").textContent="No capture selected";$("#status").className="badge";
  $("#context").textContent="Learn the report with example data, or record your first build.";
  $("#header-count").textContent="";
  $("#run").disabled=!state.runs.length;
  document.querySelectorAll("nav button").forEach(button=>{button.disabled=true;button.classList.remove("active");});
  $("#content").innerHTML='<div class="panel"><h2>Take a guided tour</h2><p class="muted">Explore a fictional C++ project, follow costly headers into compiler details, and compare example builds.</p><button id="welcome-tour" class="action">Start guided tour →</button><h3>Capture your own build</h3><pre class="evidence">buildscope record --build-dir /path/to/build\nbuildscope report latest</pre></div>';
  $("#welcome-tour").onclick=()=>BuildscopeTour.start().catch(error=>toast(error.message));
}
BuildscopeTour.bind();
$("#start-tour").disabled=true;
(async()=>{
  const requestedTour=new URLSearchParams(location.search).get("tour")==="1";
  try{
    const result=await api("/api/runs");state.runs=result.runs;state.store=result.store;
    $("#run").innerHTML=runOptions(result.initial);
    const view=location.hash.slice(1);if(["overview","headers","units","history","compare"].includes(view))state.view=view;
    if(result.initial)await loadRun(new URLSearchParams(location.search).get("run")||result.initial);
    else showWelcome();
  }catch(error){showWelcome();toast(error.message);}
  finally {$("#start-tour").disabled=false;}
  if(requestedTour)await BuildscopeTour.start();
})();
