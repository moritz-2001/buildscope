/* Global compiler investigation. SQLite owns aggregation and pagination; the
 * browser keeps only the current ranking and detail page. Example queries below
 * use synthetic normalized events and never contact the capture store. */
"use strict";
const CompilerWork = (() => {
  let request = 0, offset = 0, signature = "", current = null, searchTimer = null;
  const categories = [["templates","Templates"],["functions","Functions"],["evaluations","Constant evaluation"]];
  const category = () => state.compilerCategory || "templates";

  function shell() {
    const targets = [...new Set(state.run.project.units.map(targetFor))].sort();
    return '<p class="muted">Sum recorded compiler work across translation units, then inspect each unit’s contribution.</p>'+
      '<div class="tabs" role="tablist" aria-label="Compiler work category">'+categories.map(([key,label])=>
        '<button id="compiler-tab-'+key+'" role="tab" aria-controls="compiler-panel" aria-selected="'+(key===category())+'" class="'+(key===category()?"active":"")+'" data-compiler-category="'+key+'">'+label+'</button>').join("")+'</div>'+
      '<div class="toolbar compiler-toolbar"><input id="filter" aria-label="Search compiler spellings or source paths" placeholder="Search spellings or source paths…" value="'+esc(state.q)+'">'+
      '<select id="target-filter" aria-label="Target filter"><option value="all">All targets</option>'+targets.map(target=>'<option value="'+esc(target)+'"'+(target===state.target?' selected':"")+'>'+esc(target)+'</option>').join("")+'</select>'+
      '<select id="compiler-filter" aria-label="Compiler identity"><option value="">All compiler identities</option>'+state.run.project.compilers.map((compiler,i)=>
        '<option value="'+i+'"'+(String(i)===(state.compilerIdentity||"")?' selected':"")+'>'+esc(compiler.version.split("\n")[0])+" · #"+i+'</option>').join("")+'</select>'+
      (category()==="functions"?'<select id="compiler-activity" aria-label="Function activity"><option value="OptFunction"'+(state.compilerActivity!=="CodeGen Function"?' selected':"")+'>Optimization · OptFunction</option><option value="CodeGen Function"'+(state.compilerActivity==="CodeGen Function"?' selected':"")+'>Code generation · CodeGen Function</option></select>':"")+
      (category()==="templates"?'<label class="compiler-family"><input id="compiler-families" type="checkbox"'+(state.compilerFamilies?' checked':"")+'> Approximate families</label>':"")+'</div>'+
      '<section id="compiler-panel" role="tabpanel" aria-labelledby="compiler-tab-'+category()+'"><div id="compiler-coverage"></div>'+
      notes(["Inclusive recorded scopes can nest or overlap. Work is summed across units, not elapsed build time or predicted savings. Counts describe retained scopes and depend on trace thresholds. Compiler identities, activities, and exact source variants remain separate."])+
      '<div id="compiler-results" aria-live="polite"><p class="muted">Loading recorded compiler work…</p></div><div id="compiler-timers"></div></section>';
  }

  function filters() {
    return {run:state.run.id,category:category(),q:state.q,target:state.target==="all"?"":state.target,
      compiler:state.compilerIdentity||"",activity:category()==="functions"?(state.compilerActivity||"OptFunction"):"",
      families:category()==="templates"&&!!state.compilerFamilies,sort:["work","units","count","longest","name"].includes(state.sort)?state.sort:"work",
      descending:state.descending,limit:100};
  }

  function compilerLabel(index) {
    const compiler=state.run.project.compilers[index];
    return compiler?compiler.family+" #"+index:"Compiler #"+index;
  }

  function entityColumns(sortable=true,families=false) {
    return [
      {label:"Compiler spelling",className:"compiler-entity",...(sortable?{key:"name"}:{}),render:row=>'<span class="path">'+esc(row.name||"(no compiler spelling)")+'</span><div class="compiler-activity-inline small muted">'+esc(row.activity)+" · "+esc(compilerLabel(row.compiler))+'</div>'+
        '<div class="small muted">'+esc(families?"Approximate display family · inspect exact spellings":row.file?pathKey(row.file)+(row.line?":"+row.line:""):row.name?"Source location not supplied":"Grouped by native activity; expression identity not supplied")+'</div>'},
      {label:"Activity",className:"compiler-activity-column",render:row=>esc(row.activity)+'<div class="small muted">'+esc(compilerLabel(row.compiler))+'</div>'},
      {label:"Inclusive work",...(sortable?{key:"work"}:{}),numeric:true,tooltip:"Sum of recorded inclusive scope durations across the filtered translation units. Nested scopes can overlap.",render:row=>time(row.work_ms)},
      {label:"Units",...(sortable?{key:"units"}:{}),numeric:true,tooltip:"Distinct source/output pairs contributing recorded scopes after filtering.",render:row=>num(row.units)},
      {label:"Recorded scopes",className:"compiler-count-column",...(sortable?{key:"count"}:{}),numeric:true,render:row=>num(row.count)},
      {label:"Longest",className:"compiler-longest-column",...(sortable?{key:"longest"}:{}),numeric:true,render:row=>time(row.max_ms)}
    ];
  }

  function pager(result,prefix) {
    return '<div class="pagination"><button class="action" id="'+prefix+'-prev"'+(!result.offset?' disabled':"")+'>← Previous</button><span>'+num(result.total?result.offset+1:0)+"–"+num(Math.min(result.offset+result.limit,result.total))+" of "+num(result.total)+'</span><button class="action" id="'+prefix+'-next"'+(result.offset+result.limit>=result.total?' disabled':"")+'>Next →</button></div>';
  }

  async function load() {
    clearTimeout(searchTimer);
    const base=filters(),key=JSON.stringify(base),token=++request;
    const search=$("#filter");
    search.oninput=()=>{
      state.q=search.value;++request;current=null;
      $("#compiler-results").innerHTML='<p class="muted">Searching recorded compiler work…</p>';
      clearTimeout(searchTimer);
      searchTimer=setTimeout(()=>{if(state.view==="compiler"&&search.isConnected)load().catch(error=>toast(error.message));},180);
    };
    if(signature!==key){signature=key;offset=0;}
    document.querySelectorAll('[data-compiler-category]').forEach(button=>button.onclick=()=>{
      state.compilerCategory=button.dataset.compilerCategory;state.q="";state.sort="work";state.descending=true;render();
    });
    $("#compiler-filter").onchange=event=>{state.compilerIdentity=event.target.value;render();};
    if($("#compiler-activity"))$("#compiler-activity").onchange=event=>{state.compilerActivity=event.target.value;render();};
    if($("#compiler-families"))$("#compiler-families").onchange=event=>{state.compilerFamilies=event.target.checked;render();};
    try {
      const result=await api("/api/compiler-work",{...base,offset});
      if(token!==request||state.view!=="compiler"||JSON.stringify(filters())!==key)return;
      current={rows:result.rows,query:base};
      const coverage=result.coverage.filter(row=>base.compiler===""||String(row.compiler)===base.compiler);
      const total=coverage.reduce((sum,row)=>sum+row.units,0),traced=coverage.reduce((sum,row)=>sum+row.trace_units,0);
      $("#compiler-coverage").innerHTML='<p class="small muted">Named scope coverage: '+num(traced)+" / "+num(total)+' translation units in the selected inventory.</p><details><summary class="small">Measurement coverage</summary>'+coverage.map(row=>
        '<p class="small muted">'+esc(compilerLabel(row.compiler))+": "+num(row.profile_units)+" profiled / "+num(row.units)+" inventory units; "+num(row.trace_units)+" with named trace evidence"+(row.granularities_us!=null?"; thresholds: "+esc(row.granularities_us)+" µs":"")+".</p>").join("")+'</details>';
      $("#compiler-results").innerHTML=(result.rows.length?table(entityColumns(true,base.families),result.rows,{action:"compiler-row",id:row=>result.rows.indexOf(row)}):
        '<div class="empty">'+(traced?"No matching recorded scopes. Try a different search, target, or activity.":"Named compiler scopes are unavailable for this selection. Collect a Clang profile to measure individual entities.")+'</div>')+pager(result,"compiler");
      $("#compiler-prev").onclick=()=>{offset=Math.max(0,offset-result.limit);load().catch(error=>toast(error.message));};
      $("#compiler-next").onclick=()=>{offset+=result.limit;load().catch(error=>toast(error.message));};
      $("#compiler-timers").innerHTML=result.timers.length?'<h3>GCC native aggregate timers</h3>'+notes(["These timers are separate from named Clang scopes. Native timers may overlap; they do not identify individual templates, functions, or expressions. Search and named-activity filters apply to the named table above; native timers cover the selected target and compiler identity."])+
        table([{label:"Compiler",render:row=>esc(compilerLabel(row.compiler))},{label:"Native timer",render:row=>esc(row.name)},
          {label:"Reported wall time",numeric:true,render:row=>time(row.work_ms)},{label:"Units",numeric:true,render:row=>num(row.units)}],result.timers):"";
    } catch(error) {
      if(token===request&&state.view==="compiler"&&$("#compiler-results"))$("#compiler-results").innerHTML='<div class="empty">'+esc(error.message)+'</div>';
      throw error;
    }
  }

  async function open(index) {
    if(!current?.rows[index])return;
    await detail({type:"compiler",id:current.rows[index].key,row:current.rows[index],query:{...current.query},breakdown:"units",unit:"",offset:0});
  }

  async function detail(selection) {
    state.detail=selection;
    const {row,query,breakdown,unit}=selection,run=state.run.id;
    modal('<p class="eyebrow">GLOBAL COMPILER INVESTIGATION · '+esc(compilerLabel(row.compiler))+'</p><h2 class="detail-title">'+esc(row.name||"(no compiler spelling)")+'</h2>'+
      '<p class="detail-meta">'+esc(row.activity)+(query.families?" · approximate display family":" · exact spelling and supplied source location")+'</p>'+
      '<div class="metrics compiler-metrics">'+metric("Inclusive work",time(row.work_ms),"Sum across the selected translation units")+
      metric("Contributing units",num(row.units),"Distinct source/output pairs")+metric("Recorded scopes",num(row.count),"Longest: "+time(row.max_ms))+'</div>'+
      '<div class="toolbar"><button class="action" id="compiler-detail-units">Contributing units</button>'+(query.families?'<button class="action" id="compiler-detail-members">Exact spellings</button>':"")+
      (row.file?sourceButton(row.file,row.line):"")+'</div>'+notes(["Inclusive scopes can overlap. Matching spellings and locations are compiler observations, not proof of semantic identity across translation units."])+
      '<h3>'+({units:"Contributing translation units",members:"Exact spellings and source variants",events:"Matching recorded scopes"}[breakdown])+'</h3><div id="compiler-detail-results" aria-live="polite">Loading…</div>');
    $("#compiler-detail-units").onclick=()=>detail({...selection,breakdown:"units",unit:"",offset:0}).catch(error=>toast(error.message));
    if($("#compiler-detail-members"))$("#compiler-detail-members").onclick=()=>detail({...selection,breakdown:"members",unit:"",offset:0}).catch(error=>toast(error.message));
    const result=await api("/api/compiler-work",{...query,entity:row.key,breakdown,unit,offset:selection.offset,sort:"work",descending:true});
    if(state.run.id!==run||state.detail!==selection||!$("#detail").open||!$("#compiler-detail-results"))return;
    let columns;
    if(breakdown==="members")columns=entityColumns(false);
    else if(breakdown==="events")columns=[
      {label:"Compilation unit",render:item=>'<span class="path">'+esc(item.source_key)+'</span><div class="small muted">'+esc(item.output_key)+'</div>'},
      {label:"Compiler detail",render:item=>esc(item.name||item.activity)},
      {label:"Start · compiler-local",numeric:true,render:item=>time(item.event.start_us/1000)},
      {label:"Inclusive duration",numeric:true,render:item=>time(item.work_ms)}
    ];
    else columns=[
      {label:"Compilation unit",render:item=>'<button class="action small" data-action="unit-tab" data-id="'+esc(item.unit_id)+'" data-tab="'+query.category+'">'+esc(item.source_key)+'</button><div class="small path">'+esc(item.output_key)+'</div>'},
      {label:"Inclusive work",numeric:true,render:item=>time(item.work_ms)},
      {label:"Scopes",numeric:true,render:item=>num(item.count)},
      {label:"Longest",numeric:true,render:item=>time(item.max_ms)},
      {label:"Evidence",render:(item)=>'<button class="action small" data-work-unit="'+esc(item.unit_id)+'">Recorded scopes →</button>'}
    ];
    $("#compiler-detail-results").innerHTML=table(columns,result.rows,breakdown==="units"?null:{action:breakdown==="members"?"compiler-member":"compiler-scope",id:item=>result.rows.indexOf(item)})+pager(result,"compiler-detail");
    selection.rows=result.rows;
    $("#compiler-detail-prev").onclick=()=>detail({...selection,offset:Math.max(0,selection.offset-result.limit)}).catch(error=>toast(error.message));
    $("#compiler-detail-next").onclick=()=>detail({...selection,offset:selection.offset+result.limit}).catch(error=>toast(error.message));
    $("#compiler-detail-results").querySelectorAll('[data-work-unit]').forEach(button=>button.onclick=()=>detail({...selection,breakdown:"events",unit:button.dataset.workUnit,offset:0}).catch(error=>toast(error.message)));
  }

  async function member(index) {
    const selection=state.detail,row=selection?.rows?.[index];
    if(!row||selection.type!=="compiler")return;
    await detail({...selection,id:row.key,row,query:{...selection.query,families:false},breakdown:"units",unit:"",offset:0});
  }

  async function scope(index) {
    const selection=state.detail,row=selection?.rows?.[index];
    if(!row?.event||selection.type!=="compiler")return;
    state.detail={type:"unit",id:row.unit_id,tab:"trace"};
    await openScope(row.event.id);
  }

  return {shell,load,open,detail,member,scope};
})();

/// Synthetic counterpart of the SQLite query, used only by the guided tour.
function compilerWorkExample(run,query,eventsForUnit) {
  const target=unit=>unit.output_key.match(/CMakeFiles\/(.+?)\.dir\//)?.[1]||"(other)";
  const units=run.project.units.filter(unit=>!query.target||target(unit)===query.target);
  const coverage=[...new Set(units.map(unit=>unit.compiler))].sort().map(compiler=>{
    const inventory=units.filter(unit=>unit.compiler===compiler),profiles=run.profiles.filter(profile=>inventory.some(unit=>unit.id===profile.unit_id));
    return {compiler,units:inventory.length,profile_units:profiles.length,trace_units:profiles.filter(profile=>profile.family==="clang"&&profile.event_count>0).length,
      granularities_us:profiles.some(profile=>profile.granularity_us!=null)?[...new Set(profiles.map(profile=>profile.granularity_us).filter(value=>value!=null))].join(","):null};
  });
  const families=String(query.families)==="true"&&(query.category||"templates")==="templates";
  const matches=activity=>({templates:["InstantiateClass","InstantiateFunction"].includes(activity),functions:["OptFunction","CodeGen Function"].includes(activity),evaluations:activity.startsWith("Evaluate")||activity==="isIntegerConstantExpr"}[query.category||"templates"]);
  const groups=new Map();
  for(const unit of units) {
    const profile=run.profiles.find(profile=>profile.unit_id===unit.id);
    if(profile?.family!=="clang"||(query.compiler!==""&&query.compiler!=null&&unit.compiler!==Number(query.compiler)))continue;
    for(const event of eventsForUnit(unit.id)) {
      if(!matches(event.name)||(query.activity&&query.activity!==event.name)||!(event.detail+"\n"+(event.file||"")).toLowerCase().includes((query.q||"").toLowerCase()))continue;
      const key=JSON.stringify([unit.compiler,event.name,families?templateFamily(event.detail):event.detail,families?null:event.file??null,families?null:event.line??null]);
      if((query.entity&&query.entity!==key)||(query.unit&&query.unit!==unit.id))continue;
      const member=!!query.entity&&query.breakdown==="members",perUnit=!!query.entity&&(!query.breakdown||query.breakdown==="units"),perEvent=!!query.entity&&query.breakdown==="events";
      const groupKey=member?JSON.stringify([unit.compiler,event.name,event.detail,event.file??null,event.line??null]):perUnit?unit.id:perEvent?JSON.stringify([unit.id,event.id]):key;
      if(!groups.has(groupKey))groups.set(groupKey,{key:member?groupKey:key,compiler:unit.compiler,activity:event.name,name:member||perEvent||!families?event.detail:templateFamily(event.detail),
        file:perUnit||families&&!member&&!perEvent?null:event.file??null,line:perUnit||families&&!member&&!perEvent?null:event.line??null,
        work_ms:0,count:0,max_ms:0,units:0,unit_id:perUnit||perEvent?unit.id:null,source_key:perUnit||perEvent?unit.source_key:null,output_key:perUnit||perEvent?unit.output_key:null,event:perEvent?event:null,ids:new Set()});
      const group=groups.get(groupKey);group.work_ms+=event.duration_us/1000;group.count++;group.max_ms=Math.max(group.max_ms,event.duration_us/1000);group.ids.add(unit.id);group.units=group.ids.size;
    }
  }
  const column={work:"work_ms",units:"units",count:"count",longest:"max_ms",name:"name"}[query.sort]||"work_ms",direction=String(query.descending)==="false"?1:-1;
  const compare=(left,right)=>left<right?-1:left>right?1:0;
  const rows=[...groups.values()].map(({ids,...row})=>row).sort((a,b)=>direction*compare(a[column],b[column])||compare(a.key,b.key));
  const offset=Number(query.offset)||0,limit=Math.min(200,Math.max(1,Number(query.limit)||100));
  return {rows:rows.slice(offset,offset+limit),total:rows.length,offset,limit,coverage,timers:[]};
}

/// Approximate template-argument elision shared by unit views and tour queries.
/// Keep uncertain spellings exact, and preserve C++ angle operators and literals.
function templateFamily(name) {
  const characters=Array.from(name);
  const operators=["<=>","<<=",">>=","<<",">>","<=",">=","<",">"];
  function operatorEnd(start) {
    if(characters[start]!=="o"||characters.slice(start,start+8).join("")!=="operator"||
      start>0&&/[\p{L}\p{N}_]/u.test(characters[start-1]))return null;
    let symbol=start+8;
    while(symbol<characters.length&&/\s/u.test(characters[symbol]))symbol++;
    for(const operator of operators) {
      if(Array.from(operator).every((value,offset)=>characters[symbol+offset]===value))return symbol+operator.length;
    }
    return null;
  }
  function literalEnd(start) {
    const quote=characters[start];
    if(quote==='"'&&start>0&&characters[start-1]==="R") {
      const opening=characters.indexOf("(",start+1);
      if(opening<0||opening-start-1>16)return null;
      const delimiter=characters.slice(start+1,opening).join("");
      for(let index=opening+1;index<characters.length;index++) {
        const closing=index+1+delimiter.length;
        if(characters[index]===")"&&characters.slice(index+1,closing).join("")===delimiter&&characters[closing]==='"')return closing+1;
      }
      return null;
    }
    for(let index=start+1;index<characters.length;index++) {
      if(characters[index]===quote)return index+1;
      if(characters[index]==="\\")index++;
    }
    return null;
  }
  function argumentsEnd(start) {
    let depth=1;
    const delimiters=[];
    for(let index=start+1;index<characters.length;) {
      const operator=operatorEnd(index);
      if(operator!=null){index=operator;continue;}
      const character=characters[index];
      const separator=character==="'"&&index>0&&/^[0-9a-f]$/i.test(characters[index-1])&&/^[0-9a-f]$/i.test(characters[index+1]||"");
      if((character==="'"||character==='"')&&!separator) {
        const end=literalEnd(index);
        if(end==null)return null;
        index=end;continue;
      }
      if("([{".includes(character))delimiters.push(character);
      else if(")]}".includes(character)) {
        if(delimiters.pop()!==({")":"(","]":"[","}":"{"}[character]))return null;
      } else if(!delimiters.length&&character==="<")depth++;
      else if(!delimiters.length&&character===">") {
        depth--;
        if(!depth)return index+1;
      }
      index++;
    }
    return null;
  }
  let result="",index=0;
  while(index<characters.length) {
    const operator=operatorEnd(index);
    if(operator!=null){result+=characters.slice(index,operator).join("");index=operator;}
    else if(characters[index]==="<") {
      const end=argumentsEnd(index);
      if(end==null)return name;
      result+="<…>";index=end;
    } else result+=characters[index++];
  }
  return result;
}
