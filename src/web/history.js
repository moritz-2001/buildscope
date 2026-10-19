/* Build history: keep elapsed time aligned with measured compilation workload. */
"use strict";
const BuildHistory = (() => {
  const options = {kind:null,label:"",days:"all",baseline:null,selected:null};
  const colors = {compiled:"#8adbc0",cache_hits:"#91baf6",unknown:"#edbb7c",up_to_date:"#657681"};
  const fields = [["compiled","Compiled"],["cache_hits","Cache hits"],["unknown","Unknown"],["up_to_date","Up to date"]];
  function conditions(row) {
    return JSON.stringify([row.project_id,row.configuration_id,row.machine,row.kind,
      [...(row.targets||[])].sort(),Object.entries(row.environment||{}).sort(([a],[b])=>a.localeCompare(b)),
      row.cache_policy?.startsWith("preserved")?"preserved":row.cache_policy,row.jobs??null,[...new Set((row.instrumentation||[]).map(value=>JSON.stringify(value)))].sort()]);
  }
  function rows(runs, anchor, filters = options) {
    const kind=filters.kind||anchor.kind;
    const candidates=runs.filter(r=>r.kind===kind&&r.project_id===anchor.project_id);
    const reference=anchor.kind===kind?anchor:candidates.slice().sort((a,b)=>b.started_ms-a.started_ms)[0];
    if(!reference)return [];
    const compatible=candidates.filter(r=>conditions(r)===conditions(reference));
    const end=Math.max(...compatible.map(r=>r.started_ms),0);
    return compatible.filter(r=>(!filters.label||r.label===filters.label)&&
      (filters.days==="all"||r.started_ms>=end-Number(filters.days)*86400000))
      .sort((a,b)=>a.started_ms-b.started_ms||a.id.localeCompare(b.id));
  }
  function workloadState(before, after) {
    const a=before?.workload,b=after?.workload;
    if(!a||!b||a.unknown||b.unknown||a.up_to_date==null||b.up_to_date==null)return "Workload coverage incomplete";
    if(fields.some(([key])=>a[key]!==b[key])||a.total_units!==b.total_units||before.actions!==after.actions||
      JSON.stringify(a.executed_units)!==JSON.stringify(b.executed_units))return "Workload changed";
    return "Same recorded workload";
  }
  function delta(before, after) {
    if(!before||!after||before.status!=="success"||after.status!=="success")return null;
    const ms=after.elapsed_ms-before.elapsed_ms;
    return {ms,percent:before.elapsed_ms>0?ms/before.elapsed_ms*100:null};
  }
  function counts(workload) {
    return fields.map(([key,label])=>'<span class="workload-count"><i class="'+key+'"></i>'+esc(label)+
      ' <strong>'+ (workload?.[key]==null?"Unknown":num(workload[key]))+'</strong></span>').join("");
  }
  function shell() {
    const anchor=state.runs.find(r=>r.id===state.run.id)||state.runs[0];
    if(!anchor)return '<div class="empty">No recorded builds yet.</div>';
    const selectedRows=rows(state.runs,anchor);
    const successful=selectedRows.filter(r=>r.status==="success"&&Number.isFinite(r.elapsed_ms));
    const baseline=successful.find(r=>r.id===options.baseline)||successful[0];
    const selected=selectedRows.find(r=>r.id===options.selected)||successful.at(-1)||selectedRows.at(-1);
    const difference=delta(baseline,selected);
    const labels=[...new Set(state.runs.filter(r=>r.project_id===anchor.project_id&&r.kind===(options.kind||anchor.kind)).map(r=>r.label).filter(Boolean))].sort();
    const select=(id,label,values,current)=>'<label>'+label+'<select id="'+id+'">'+values.map(([value,text])=>'<option value="'+esc(value)+'"'+(value===current?' selected':"")+'>'+esc(text)+'</option>').join("")+'</select></label>';
    return '<div id="history-panel"><p class="muted">Elapsed time and workload for matching recorded conditions. Ordinary builds, instrumented profiles, and launcher spans are separate series.</p>'+
      '<div class="toolbar history-filters">'+select("history-kind","Capture mode",[["record","Ordinary builds"],["profile","Instrumented profiles"],["launcher","Launcher spans"]],options.kind||anchor.kind)+
      select("history-label","Scenario",[["","All labels"],...labels.map(l=>[l,l])],options.label)+
      select("history-days","Date range",[["all","All history"],["7","Last 7 days"],["30","Last 30 days"],["90","Last 90 days"]],options.days)+
      select("history-baseline","Baseline",successful.map(r=>[r.id,date(r.started_ms)+" · "+(r.label||r.id)]),baseline?.id)+'</div>'+
      '<p class="small muted">Series matches project, build configuration, machine, requested targets, environment, cache policy, instrumentation, and requested parallelism. Date ranges end at the latest recorded capture.</p>'+
      (selected?'<div class="metrics">'+metric(selected.kind==="launcher"?"Observed compiler span":"Selected elapsed time",time(selected.elapsed_ms),date(selected.started_ms)+" · "+selected.status)+
        metric("Elapsed difference",difference?(difference.ms<0?"−":difference.ms>0?"+":"")+time(Math.abs(difference.ms)):"Unavailable",difference?.percent!=null?(difference.percent>0?"+":"")+difference.percent.toFixed(1)+"% vs baseline":"Requires successful captures and a nonzero baseline")+
        metric("Workload comparison",workloadState(baseline,selected),"Elapsed changes alone do not establish faster compilation")+'</div>'+
         '<div class="history-selection"><div class="workload-counts" aria-label="Selected compilation workload">'+counts(selected.workload)+'</div><p class="small muted">'+
        (selected.workload?num(selected.workload.source_files)+" distinct executed source files · "+num(selected.workload.executed_units.length)+" executed compilation units · "+(selected.workload.total_units==null?"requested scope unknown":num(selected.workload.total_units)+" units in requested scope"):
          "This capture predates cache-outcome collection. Executed build actions cannot be treated as confirmed compiler executions.")+'</p>'+
        (selected.workload?.notes?.length?'<details class="small muted"><summary>Measurement coverage</summary>'+notes(selected.workload.notes)+'</details>':"")+
        '<div class="toolbar"><button class="action" data-action="run" data-id="'+esc(selected.id)+'">Open capture</button>'+
        (selected.status==="success"?'<button class="action" data-action="history-baseline" data-id="'+esc(selected.id)+'">Set baseline</button>':"")+
        (baseline&&selected.status==="success"?'<button class="action" data-action="history-compare" data-before="'+esc(baseline.id)+'" data-id="'+esc(selected.id)+'">Compare with baseline</button>':"")+'</div></div>':"")+
      '<div class="panel"><h2>Duration and compilation workload</h2><p class="small muted">Both plots share evenly spaced positions in capture order. Counts are source/output pairs; up-to-date units are separate from cache hits. Failed and interrupted captures do not contribute to the duration line.</p>'+
      '<div class="workload-counts">'+fields.map(([key,label])=>'<span class="workload-count"><i class="'+key+'"></i>'+label+'</span>').join("")+'</div>'+
      (selectedRows.length?'<div class="history-scroll">'+chart(selectedRows,baseline,selected)+'</div>':'<div class="empty">No matching captures. Change the mode, scenario, or date range.</div>')+'</div>'+
      table([{label:"Capture",render:r=>'<span class="path">'+esc(r.label||r.id)+'</span><div class="small muted">'+esc(date(r.started_ms))+'</div>'},
        {label:"Status",render:r=>esc(r.status)}, {label:"Duration",numeric:true,render:r=>time(r.elapsed_ms)},
        ...fields.map(([key,label])=>({label,numeric:true,render:r=>r.workload?.[key]==null?"Unknown":num(r.workload[key])})),
        {label:"Git",render:r=>gitDetails(r.git)}],selectedRows.slice().reverse(),{action:"history-select",id:r=>r.id})+'</div>';
  }
  function chart(captures,baseline,selected) {
    const width=Math.max(660,captures.length*94+90),left=66,right=width-30;
    const x=i=>captures.length===1?(left+right)/2:left+(right-left)*i/(captures.length-1);
    const max=Math.max(...captures.filter(r=>r.status==="success").map(r=>r.elapsed_ms),1)*1.12;
    const y=ms=>200-ms/max*155;
    let svg='<svg class="history-graph" viewBox="0 0 '+width+' 450" width="'+width+'" role="group" aria-label="Elapsed duration above, compilation workload below. Select a capture point or use the history table.">';
    for(let i=0;i<=4;i++) {
      const ms=max*i/4,py=y(ms);
      svg+='<line x1="'+left+'" x2="'+right+'" y1="'+py+'" y2="'+py+'" class="history-grid"/><text x="'+(left-8)+'" y="'+(py+4)+'" text-anchor="end">'+esc(time(ms))+'</text>';
    }
    if(baseline)svg+='<line x1="'+left+'" x2="'+right+'" y1="'+y(baseline.elapsed_ms)+'" y2="'+y(baseline.elapsed_ms)+'" class="history-baseline"/><text x="'+left+'" y="24">Baseline: '+esc(time(baseline.elapsed_ms))+'</text>';
    let segment=[];
    const flush=()=>{if(segment.length>1)svg+='<polyline points="'+segment.join(" ")+'" class="history-line"/>';segment=[];};
    captures.forEach((r,i)=>{if(r.status==="success"&&Number.isFinite(r.elapsed_ms))segment.push(x(i)+","+y(r.elapsed_ms));else flush();});flush();
    const maxCount=Math.max(...captures.map(r=>r.workload?fields.reduce((sum,[key])=>sum+(r.workload[key]||0),0):0),1);
    svg+='<text x="'+left+'" y="246">Compilation units · stacked counts</text>';
    captures.forEach((r,i)=>{
      const px=x(i),w=r.workload,py=r.status==="success"?y(r.elapsed_ms):211;
      const description=date(r.started_ms)+" · "+(r.label||r.id)+" · "+r.status+" · "+time(r.elapsed_ms)+" · "+fields.map(([key,label])=>label+": "+(w?.[key]??"unknown")).join(" · ");
      svg+='<a href="#history" role="button" data-action="history-select" data-id="'+esc(r.id)+'" aria-label="'+esc(description)+'"'+(r.id===selected?.id?' aria-current="true"':"")+'><title>'+esc(description)+'</title><rect x="'+(px-35)+'" y="28" width="70" height="400" class="history-hit"/>'+
        '<circle cx="'+px+'" cy="'+py+'" r="'+(r.id===selected?.id?7:5)+'" class="history-point '+(r.status==="success"?"":"history-failed")+'"/>';
      if(w) {
        let bottom=380;
        for(const [key] of fields){const h=(w[key]||0)/maxCount*112;svg+='<rect x="'+(px-19)+'" y="'+(bottom-h)+'" width="38" height="'+h+'" fill="'+colors[key]+'"/>';bottom-=h;}
        svg+='<text x="'+px+'" y="'+(bottom-8)+'" text-anchor="middle">'+num(w.compiled)+" / "+num(w.cache_hits)+" / "+num(w.unknown)+'</text>';
        svg+='<text x="'+px+'" y="398" text-anchor="middle">'+(w.up_to_date==null?"? current":num(w.up_to_date)+" current")+'</text>';
      } else svg+='<text x="'+px+'" y="330" text-anchor="middle">Unknown</text>';
      svg+='<text x="'+px+'" y="420" text-anchor="middle">'+esc(new Date(r.started_ms).toLocaleDateString())+'</text><text x="'+px+'" y="438" text-anchor="middle">'+esc(new Date(r.started_ms).toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"}))+'</text></a>';
    });
    return svg+'</svg><p class="small muted">Bar labels: compiled / cache hits / unknown. “Current” means up to date within the requested build scope. Unknown coverage is never represented as zero.</p>';
  }
  function bind() {
    for(const [id,key] of [["history-kind","kind"],["history-label","label"],["history-days","days"],["history-baseline","baseline"]]) {
      const control=$("#"+id);if(control)control.onchange=event=>{options[key]=event.target.value;if(key!=="baseline")options.selected=null;if(key==="kind")options.label="";render();};
    }
  }
  function select(id) {options.selected=id;render();}
  function baseline(id) {options.baseline=id;render();}
  return {shell,bind,select,baseline,rows,conditions,workloadState,delta};
})();
