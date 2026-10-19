/* Guided report walkthrough and synthetic evidence. Examples live only in browser
 * memory and use the report's normal views and detail interfaces. */
"use strict";
const BuildscopeTour = (() => {
  let active = false, saved = null, step = 0, moving = false;
  const example = createExampleData();
  const steps = [
    {view:"overview",target:"#content .metrics",title:"Read the build at a glance",text:"This fictional Atlas viewer took 12 seconds to profile. Its 20 seconds of accumulated action work overlap across three concurrent compilations. Elapsed time, work, and concurrency answer different questions.",hint:"All paths, timings, and evidence in this tour are examples. No build is executed."},
    {view:"overview",target:"#build-timeline",title:"Follow work through time",text:"Each bar is an executed action. Compilation bars overlap; the blue link action waits for its inputs. Hover for a path and duration, or click a compilation to open its investigation.",hint:"The dependency chain ignores resource constraints; it does not predict the time an edit will save."},
    {view:"overview",target:"#content .grid",title:"Turn evidence into an experiment",text:"Findings suggest where to investigate. Here, math.hpp has six seconds of inclusive processing across two units. Expand its evidence and next experiment, or open the finding. Capture coverage explains which measurements are available.",hint:"Inclusive measurements can overlap. A costly header is a candidate to investigate, not a guaranteed saving."},
    {view:"headers",target:"#content .toolbar",title:"Find headers with reach and cost",text:"Search paths, choose a target, or narrow the path scope. Click a column heading to sort. math.hpp is expensive in two units; scene.hpp reaches three. Reach, measured processing, and associated rebuild work are separate measures.",hint:"Try searching for math, then clear the search. Click any header row for its include paths and dependent units."},
    {view:"headers",detail:"header",target:"#include-paths",title:"Explain why a header matters",text:"math.hpp is included through scene.hpp by render.cpp and scene.cpp. The include chains explain how it enters each compilation. Distribution, rebuild evidence, and history help distinguish repeated cost from a single expensive invocation.",hint:"Source previews and dependent-unit rows are clickable. Native triggers and changed contributors have different meanings."},
    {view:"compiler",target:"#compiler-panel",title:"Find work across translation units",text:"Compiler work sums recorded inclusive durations across the selected translation units. Inspect template spellings, function optimization, or constant evaluations. Select a row to see each unit’s contribution and its matching recorded scopes.",hint:"Try approximate template families or a target filter. Overlapping inclusive scopes are not elapsed build time or predicted savings."},
    {view:"units",target:"#content .table-wrap",title:"Choose an expensive compilation",text:"A compilation unit is a source/output pair. render.cpp is the slowest example at nine seconds. Frontend, backend, and template activity come from compiler measurements and can overlap.",hint:"Filter by source or unit ID, select a target, sort by action duration, and open a row."},
    {view:"units",detail:"summary",target:"#unit-detail",title:"Inspect compiler activity",text:"The summary shows recorded phases and detail coverage for render.cpp. Tabs separate templates, functions, constant evaluation, includes, trace events, and the original command. Real captures can retain raw evidence and provide a focused profiling command.",hint:"This sample profile uses Clang. GCC supplies aggregate timers rather than individual template or header timings."},
    {view:"units",detail:"templates",target:"#unit-detail",title:"Identify costly template spellings",text:"Mesh<Vertex>::upload() accounts for 1.8 seconds of inclusive recorded work. Compare exact spellings, counts, and longest events. Approximate family grouping helps browse similar specializations.",hint:"Try the grouping checkbox or a source link. Inclusive template timings must not be added as independent elapsed time."},
    {view:"units",detail:"trace",target:"#unit-detail",title:"Zoom into recorded scopes",text:"Search events, filter by compiler activity, or switch to chronological order. Open an event to inspect its immediate recorded children and containing scope. These times are local to one compiler invocation.",hint:"Try filtering for Mesh, or opening the Frontend event. Time containment is not a semantic template dependency graph."},
    {view:"history",target:"#content",title:"Keep measurement modes in context",text:"The example history contains three ordinary builds before a change, three after, and a separate instrumented profile. Select a chart point or table row, then open its capture. The duration plot aligns with compiled, cached, unknown, and up-to-date unit counts. Choose a baseline and inspect workload changes.",hint:"Profiles add instrumentation overhead. Compare ordinary builds with ordinary builds under matching conditions."},
    {view:"compare",target:"#comparison",title:"Check a change with repeated builds",text:"The first ordinary build falls from 8.0 to 5.4 seconds after reducing header work. Matched units and headers show where measurements differ. Switch to repeated measurements by label to see medians, spread, and sample counts.",hint:"The three before samples have an 8.0-second median; after has 5.6 seconds. Matching conditions support comparison, but do not prove causality."},
    {view:"overview",target:"#content .metrics",title:"Ready to investigate your build",text:"Start with elapsed time and coverage, follow a finding or costly header into its units, then use compiler detail to design an experiment. Record repeated before/after builds to check the result.",hint:"Finish returns to your original capture and filters. You can replay this tour anytime from the sidebar."}
  ];

  async function start() {
    if (active) return;
    saved = {state:{...state,detail:state.detail?{...state.detail}:null},url:location.href,
      scroll:window.scrollY,focus:document.activeElement};
    active = true;
    state.runs = example.runs;
    state.store = "example-store";
    state.detail = null;
    document.body.classList.add("tour-active");
    $("#start-tour").disabled = true;
    $("#run").innerHTML = runOptions(example.profile.id);
    await showStep(0);
  }
  async function showStep(index) {
    if (moving || !active) return;
    moving = true;
    step = index;
    const current = steps[step];
    const panel = $("#tour-panel");
    panel.hidden = false;
    $("#tour-back").disabled = true;
    $("#tour-next").disabled = true;
    document.querySelectorAll(".tour-highlight").forEach(node=>node.classList.remove("tour-highlight"));
    $("#detail").close();
    state.detail = null;
    state.view = current.view;
    state.q = ""; state.scope = "all"; state.target = "all";
    state.sort = current.view === "units" ? "work" : "processing_ms";
    state.descending = true;
    try {
      await loadRun(current.view === "compare" ? "example-after-1" : example.profile.id);
      if (!active) return;
      if (current.detail === "header") await openHeader(example.headerPath);
      else if (current.detail) await openUnit("example-render",current.detail);
      else if (current.view === "compare") {
        $("#before-run").value = "example-before-1";
        $("#after-run").value = "example-after-1";
        await showComparison();
      }
      if (!active) return;
      $("#tour-title").textContent = current.title;
      $("#tour-description").textContent = current.text;
      $("#tour-hint").textContent = current.hint;
      $("#tour-progress").textContent = `${step+1} of ${steps.length}`;
      $("#tour-announcement").textContent = `Step ${step+1} of ${steps.length}: ${current.title}. ${current.text}`;
      $("#tour-back").disabled = step === 0;
      $("#tour-next").disabled = false;
      $("#tour-next").textContent = step === steps.length-1 ? "Finish tour" : "Next →";
      mount();
      const target = $(current.target);
      target?.classList.add("tour-highlight");
      target?.scrollIntoView({block:"start"});
      $("#tour-next").focus({preventScroll:true});
    } catch (error) {
      toast(error.message);
      await exit();
    } finally {
      moving = false;
    }
  }
  function mount() {
    if (!active) return;
    const parent = $("#detail").open ? $("#detail") : document.body;
    parent.append($("#tour-panel"));
  }
  async function exit() {
    if (!active) return;
    const previous = saved;
    active = false;
    ++generation; // Invalidate any pending load from the walkthrough.
    $("#detail").close();
    document.body.append($("#tour-panel"));
    $("#tour-panel").hidden = true;
    document.body.classList.remove("tour-active");
    document.querySelectorAll(".tour-highlight").forEach(node=>node.classList.remove("tour-highlight"));
    $("#start-tour").disabled = false;
    Object.assign(state,previous.state,{detail:null});
    $("#run").innerHTML = runOptions(state.run?.id);
    if (state.run) {
      // Rebuild indexes from the saved capture without another server request.
      indexRun(state.run);
      render();
      const detail = previous.state.detail;
      if (detail?.type === "unit") await openUnit(detail.id,detail.tab);
      else if (detail?.type === "header") await openHeader(detail.id);
      else if (detail?.type === "compiler") await CompilerWork.detail(detail);
    } else {
      showWelcome();
    }
    history.replaceState(null,"",previous.url);
    window.scrollTo(0,previous.scroll);
    (previous.focus?.isConnected&&previous.focus!==document.body?previous.focus:$("#start-tour"))?.focus({preventScroll:true});
    saved = null;
  }
  function request(path, query) {
    // Never fall through to the capture server while examples are active.
    return structuredClone(example.request(path,query));
  }
  function bind() {
    $("#start-tour").onclick = ()=>start().catch(error=>toast(error.message));
    $("#exit-tour").onclick = ()=>exit().catch(error=>toast(error.message));
    $("#tour-back").onclick = ()=>showStep(step-1);
    $("#tour-next").onclick = ()=>step === steps.length-1 ? exit().catch(error=>toast(error.message)) : showStep(step+1);
    $("#detail").addEventListener("close",mount);
    document.addEventListener("keydown",event=>{
      if (!active) return;
      if (event.key === "Escape") {
        event.preventDefault(); event.stopImmediatePropagation();
        exit().catch(error=>toast(error.message));
      } else if ($("#tour-panel").contains(event.target) && ["ArrowLeft","ArrowRight"].includes(event.key)) {
        event.preventDefault();
        if (event.key === "ArrowLeft" && step > 0) showStep(step-1);
        if (event.key === "ArrowRight") $("#tour-next").click();
      }
    },true);
  }
  return {get active(){return active;},start,request,mount,bind};
})();

function createExampleData() {
  const source = "/example/atlas", build = "/example/atlas/build";
  const headerPath = source+"/include/math.hpp";
  const units = ["render","scene","main"].map(name=>({id:"example-"+name,source:source+"/src/"+name+".cpp",
    source_key:"src/"+name+".cpp",output_key:"CMakeFiles/viewer.dir/src/"+name+".cpp.o",compiler:0,
    directory:build,arguments:["clang++","-O2","-I"+source+"/include","-c",source+"/src/"+name+".cpp"],response_files:{}}));
  const profile = {
    id:"example-profile",label:"Example · compiler investigation",kind:"profile",status:"success",started_ms:1792317600000,
    elapsed_ms:12000,jobs:4,machine:"example-host",environment:{},cache_policy:"bypassed in isolated example profile",targets:["viewer"],
    project:{project_id:"example-atlas",source_dir:source,build_dir:build,build_type:"Release",configuration_id:"example-clang-release",units,
      compilers:[{family:"clang",version:"Clang 20.1.8 · illustrative example"}]},
    actions:[{kind:"generate",start_ms:0,end_ms:400,outputs:[build+"/generated/config.hpp"]},
      ...units.map((u,i)=>({kind:"compile",unit_id:u.id,start_ms:400,end_ms:[9400,6400,2400][i],outputs:[build+"/"+u.output_key]})),
      {kind:"link",start_ms:9400,end_ms:12000,outputs:[build+"/viewer"]}],
    summary:{work_ms:20000,observed_units:3,max_concurrency:3,dependency_path_ms:12000,dependency_units:3},
    profiles:[],headers:[],findings:[],
    notes:["Synthetic example data for the guided tour; no compiler was run.","This instrumented profile is separate from the ordinary before/after recordings."]
  };
  const costs = [6000,7500,1000];
  profile.headers = ["include/math.hpp","include/scene.hpp","<external>/vector"].map((key,i)=>({key,
    path:i===2?"/example/sdk/vector":source+"/"+key,units:units.slice(0,i===0?2:3).map(u=>u.id),
    direct_units:i===0?[]:units.map(u=>u.id),processing_ms:costs[i],recorded_self_ms:i===1?1500:costs[i]*0.6,
    measured_units:i===0?2:3,samples_ms:i===0?[2400,3600]:i===1?[1000,2500,4000]:[200,350,450],
    rebuilt_units:i===0?2:3,rebuilt_work_ms:i===0?15000:17000,changed_contributor:false,explicit_trigger:false,
    evidence:[],history:{record:{captures:6,trigger_runs:0,contributor_runs:0}}}));
  profile.findings = [{origin:"compiler",subject:headerPath,title:"math.hpp repeats expensive processing",
    explanation:"6.00 s of inclusive processing was recorded across two compilation units.",
    evidence:["render.cpp: 3.60 s; scene.cpp: 2.40 s.","Both include math.hpp through scene.hpp. Timings include nested work."],
    experiment:"Move the implementation-only math include out of scene.hpp, then record repeated ordinary builds."}];
  profile.profiles = units.map((unit,i)=>({unit_id:unit.id,family:"clang",artifact:"example/trace.json",event_count:9,granularity_us:0,
    phases:[{name:"Frontend",unit:"ms",value:[7000,4500,1400][i],coverage:"Example recorded invocation"},
      {name:"Backend",unit:"ms",value:[1800,1300,500][i],coverage:"Example recorded invocation"},
      {name:"InstantiateFunction",unit:"ms",value:[1800,1000,200][i],coverage:"Inclusive example scopes"}],
    templates:[{name:"Mesh<Vertex>::upload()",total_ms:[1800,1000,200][i],count:2,max_ms:[1200,700,150][i],file:headerPath,line:4}],
    functions:[{name:unit.source_key.replace("src/","").replace(".cpp","()"),total_ms:[1400,1000,350][i],count:1,max_ms:[1400,1000,350][i],file:unit.source,line:3}],
    evaluations:[{name:"EvaluateAsConstantExpr",total_ms:120,count:3,max_ms:60,file:headerPath,line:3}],
    header_costs:profile.headers.filter(h=>h.units.includes(unit.id)).map(h=>({path:h.path})),
    includes:[{parent:unit.source,child:source+"/include/scene.hpp",depth:1},
      ...(i<2?[{parent:source+"/include/scene.hpp",child:headerPath,depth:2}]:[]),
      {parent:unit.source,child:"/example/sdk/vector",depth:1}],
    notes:["Illustrative Clang measurements. Inclusive phases and named scopes overlap."]}));
  const captures = [profile];
  for (const [label,timings] of [["before",[8000,7800,8200]],["after",[5400,5600,5800]]]) {
    timings.forEach((elapsed,i)=>{
      const run = structuredClone(profile), after = label === "after";
      Object.assign(run,{id:`example-${label}-${i+1}`,label:"Example · "+label,kind:"record",elapsed_ms:elapsed,
        started_ms:1792306800000+(after?3600000:0)+i*600000,profiles:[],cache_policy:"preserved",findings:[],
        notes:["Synthetic ordinary build. Compiler profiling was disabled; individual header processing is unavailable."]});
      run.actions = [{kind:"generate",start_ms:0,end_ms:400,outputs:[build+"/generated/config.hpp"]},
        ...units.map((u,j)=>({kind:"compile",unit_id:u.id,start_ms:400,end_ms:400+[elapsed-3000,after?2200:3200,after?900:1200][j],outputs:[build+"/"+u.output_key]})),
        {kind:"link",start_ms:elapsed-2600,end_ms:elapsed,outputs:[build+"/viewer"]}];
      run.summary.work_ms = run.actions.reduce((sum,a)=>sum+a.end_ms-a.start_ms,0);
      run.summary.dependency_path_ms = elapsed;
      run.headers.forEach(h=>{h.processing_ms=null;h.recorded_self_ms=null;h.samples_ms=[];h.measured_units=0;
        h.rebuilt_work_ms=run.actions.filter(a=>h.units.includes(a.unit_id)).reduce((sum,a)=>sum+a.end_ms-a.start_ms,0);});
      captures.push(run);
    });
  }
  // Fictional outcomes exercise the same workload report contract as real captures.
  for(const run of captures)run.workload={compiled:3,cache_hits:0,unknown:0,up_to_date:0,total_units:3,
    source_files:3,executed_units:units.map(u=>u.id).sort(),notes:["Synthetic workload evidence for the guided tour."]};
  const runs = [...captures].sort((a,b)=>b.started_ms-a.started_ms).map(r=>({id:r.id,label:r.label,kind:r.kind,status:r.status,
    started_ms:r.started_ms,elapsed_ms:r.elapsed_ms,configuration_id:r.project.configuration_id,actions:r.actions.length,
    project_id:r.project.project_id,machine:r.machine,environment:r.environment,targets:r.targets,
    cache_policy:r.cache_policy,jobs:r.jobs,workload:r.workload,instrumentation:r.profiles.map(p=>[p.granularity_us,p.verbose||false])}));
  function capture(id) {
    const run = captures.find(r=>r.id===id);
    if (!run) throw new Error("Example capture unavailable.");
    return run;
  }
  function events(unit) {
    const p = profile.profiles.find(p=>p.unit_id===unit);
    if (!p) return [];
    const index=units.findIndex(u=>u.id===unit),header=index<2?headerPath:source+"/include/scene.hpp";
    return [{id:0,parent:null,name:"Frontend",detail:unit,start_us:0,duration_us:p.phases[0].value*1000,depth:0,thread:"1"},
      {id:1,parent:0,name:"Source",detail:header.slice(source.length+1),start_us:100000,duration_us:[3600000,2400000,1000000][index],depth:1,thread:"1",file:header,line:1},
      {id:2,parent:1,name:"InstantiateFunction",detail:"Mesh<Vertex>::upload()",start_us:200000,duration_us:p.templates[0].max_ms*1000,depth:2,thread:"1",file:headerPath,line:4},
      {id:3,parent:1,name:"InstantiateFunction",detail:"Mesh<Vertex>::upload()",start_us:200000+p.templates[0].max_ms*1000,duration_us:(p.templates[0].total_ms-p.templates[0].max_ms)*1000,depth:2,thread:"1",file:headerPath,line:4},
      ...[60,40,20].map((duration,i)=>({id:4+i,parent:1,name:"EvaluateAsConstantExpr",detail:"",start_us:200000+p.templates[0].total_ms*1000+i*60000,duration_us:duration*1000,depth:2,thread:"1",file:headerPath,line:3})),
      {id:7,parent:null,name:"Backend",detail:"Code generation",start_us:p.phases[0].value*1000,duration_us:p.phases[1].value*1000,depth:0,thread:"1"},
      {id:8,parent:7,name:"OptFunction",detail:p.functions[0].name,start_us:p.phases[0].value*1000,duration_us:p.functions[0].total_ms*1000,depth:1,thread:"1",file:units[index].source,line:3}];
  }
  function request(path,q={}) {
    if (path === "/api/run") return capture(q.id);
    if (path === "/api/compare" || path === "/api/compare-series") {
      const limitations = ["Synthetic measurements illustrate the interface. Repeat real experiments under matching conditions."];
      if (path.endsWith("series")) {
        const series = label=>{
          const values=captures.filter(r=>r.label===label).map(r=>r.elapsed_ms).sort((a,b)=>a-b);
          if (!values.length) throw new Error("Example experiment label unavailable.");
          const percentile=p=>{const at=(values.length-1)*p,low=Math.floor(at);return values[low]+(values[Math.ceil(at)]-values[low])*(at-low);};
          return {label,count:values.length,median_ms:percentile(.5),p10_ms:percentile(.1),
            p90_ms:percentile(.9),min_ms:values[0],max_ms:values.at(-1)};
        };
        const before=series(q.before_label),after=series(q.after_label);
        const modes=new Set(captures.filter(r=>[q.before_label,q.after_label].includes(r.label)).map(r=>r.kind));
        if(modes.size>1)limitations.push("Capture modes and instrumentation differ.");
        if(Math.min(before.count,after.count)<2)limitations.push("One sample does not establish measurement variability.");
        return {before,after,delta_median_ms:after.median_ms-before.median_ms,conditions_match:modes.size===1,limitations};
      }
      const before=capture(q.before),after=capture(q.after);
      const work=(run,id)=>run.actions.find(a=>a.unit_id===id).end_ms-400;
      return {before_elapsed_ms:before.elapsed_ms,after_elapsed_ms:after.elapsed_ms,
        delta_elapsed_ms:after.elapsed_ms-before.elapsed_ms,before_timing_scope:"Example Ninja execution",after_timing_scope:"Example Ninja execution",
        conditions_match:before.kind===after.kind,differences:before.kind===after.kind?[]:["Capture modes and instrumentation differ."],limitations,
        units:units.map(u=>({source:u.source_key,before_ms:work(before,u.id),after_ms:work(after,u.id),delta_ms:work(after,u.id)-work(before,u.id)})),
        headers:before.headers.map((h,i)=>({key:h.key,before_units:h.units.length,after_units:after.headers[i].units.length,
          before_processing_ms:h.processing_ms,after_processing_ms:after.headers[i].processing_ms,
          delta_processing_ms:h.processing_ms==null||after.headers[i].processing_ms==null?null:after.headers[i].processing_ms-h.processing_ms}))};
    }
    const run = capture(q.run);
    if (path === "/api/compiler-work") return compilerWorkExample(run,q,events);
    if (path === "/api/unit") return {unit:units.find(u=>u.id===q.unit),profile:run.profiles.find(p=>p.unit_id===q.unit)||null};
    if (path === "/api/header") return run.headers.find(h=>h.path===q.path);
    if (path === "/api/include-paths") {
      const h=run.headers.find(h=>h.path===q.path);
      const paths=h.units.map(id=>({units:1,chain:[units.find(u=>u.id===id).source,
        ...(q.path===headerPath?[source+"/include/scene.hpp"]:[]),q.path]}));
      return {paths,total:paths.length};
    }
    if (path === "/api/header-history") return captures.map(r=>({started_ms:r.started_ms,kind:r.kind,
      explicit_trigger:false,changed_contributor:false,rebuilt_units:r.headers.find(h=>h.key===q.key)?.rebuilt_units||0,
      rebuilt_work_ms:r.headers.find(h=>h.key===q.key)?.rebuilt_work_ms||0}));
    if (path === "/api/events") {
      const all=run.profiles.length?events(q.unit):[];
      const filtered=all.filter(e=>(!q.kind||e.name===q.kind)&&
        (e.name+" "+e.detail).toLowerCase().includes((q.q||"").toLowerCase()))
        .sort((a,b)=>q.sort==="time"?a.start_us-b.start_us:b.duration_us-a.duration_us);
      return {names:[...new Set(all.map(e=>e.name))],total:filtered.length,events:filtered.slice(Number(q.offset)||0,(Number(q.offset)||0)+(Number(q.limit)||100))};
    }
    if (path === "/api/scope") {
      const all=events(q.unit);
      return {root:all.find(e=>e.id===Number(q.id)),children:all.filter(e=>e.parent===Number(q.id))};
    }
    if (path === "/api/source") return {matches_capture:true,text:q.path.endsWith("math.hpp")?
      "// Fictional example source used by the guided tour.\n#pragma once\nconstexpr int vertex_count = 1024;\ntemplate<class Vertex> struct Mesh { void upload(); };\n":
      q.path.endsWith("scene.hpp")?"// Fictional example source used by the guided tour.\n#pragma once\n#include \"math.hpp\"\nstruct Scene { Mesh<float> mesh; };\n":
      "// Fictional example source used by the guided tour.\n#include \"scene.hpp\"\nvoid draw(Scene& scene) { scene.mesh.upload(); }\n"};
    throw new Error("This action is unavailable in the example tour.");
  }
  return {profile,runs,headerPath,request};
}
