// Exercise the history contract independently of canvas, browser timing, and live stores.
const {test}=require('node:test');
const assert=require('node:assert/strict');
const {readFileSync}=require('node:fs');
const vm=require('node:vm');
function history() {
  const context=vm.createContext({structuredClone,URLSearchParams,Intl,Date,Number,Set,Map,
    esc:value=>String(value??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])),
    num:value=>String(value??0),time:value=>value==null?'Unavailable':value+' ms',signed:value=>String(value),date:value=>new Date(value).toISOString(),
    metric:(label,value,caption)=>`<div>${label}: ${value} ${caption}</div>`,notes:items=>items.join(' '),table:()=>'',gitDetails:()=>'',state:{}});
  vm.runInContext(readFileSync(require.resolve('../src/web/history.js'),'utf8')+';globalThis.history=BuildHistory;',context);
  return context;
}
function run(id,overrides={}) {
  return {id,started_ms:Number(id)*1000,elapsed_ms:1000,status:'success',kind:'record',label:'clean',
    project_id:'project',configuration_id:'config',machine:'host',targets:['app'],environment:{A:'1',B:'2'},
    cache_policy:'preserved',jobs:4,instrumentation:[],actions:3,
    workload:{compiled:3,cache_hits:0,unknown:0,up_to_date:0,total_units:3,source_files:3,executed_units:['a','b','c'],notes:[]},...overrides};
}
test('series separate every recorded measurement condition and preserve failed markers',()=>{
  const {history:h}=history(),anchor=run('1');
  const incompatible=[{project_id:'other'},{configuration_id:'other'},{machine:'other'},{kind:'profile'},
    {targets:['tests']},{environment:{A:'3'}},{cache_policy:'bypassed'},{jobs:1},{instrumentation:[[0,true]]}];
  const captures=[anchor,run('2',{status:'failed'}),...incompatible.map((change,i)=>run(String(i+3),change))];
  assert.deepEqual(Array.from(h.rows(captures,anchor,{days:'all'}),r=>r.id),['1','2']);
  assert.equal(h.conditions(anchor),h.conditions(run('9',{environment:{B:'2',A:'1'}})));
  assert.equal(h.conditions(anchor),h.conditions(run('9',{cache_policy:'preserved; individual outcomes unknown'})));
  assert.equal(h.conditions(run('9',{instrumentation:[[0,true],[0,true]]})),h.conditions(run('9',{instrumentation:[[0,true]]})));
});
test('scenario and time filters are deterministic and missing series are empty',()=>{
  const {history:h}=history(),a=run('1',{started_ms:1});
  const b=run('2',{started_ms:10*86400000,label:'edit'});
  assert.deepEqual(Array.from(h.rows([b,a],a,{days:'7'}),r=>r.id),['2']);
  assert.deepEqual(Array.from(h.rows([b,a],a,{days:'all',label:'clean'}),r=>r.id),['1']);
  assert.equal(h.rows([a,b],a,{days:'all',kind:'launcher'}).length,0);
});
test('cache reuse, scope, rebuilt identities, and unknown coverage prevent false improvement claims',()=>{
  const {history:h}=history(),a=run('1'),b=run('2');
  assert.equal(h.workloadState(a,b),'Same recorded workload');
  for(const change of [{compiled:1,cache_hits:2},{up_to_date:2},{total_units:5},{executed_units:['d','e','f']}]) {
    assert.equal(h.workloadState(a,{...b,workload:{...b.workload,...change}}),'Workload changed');
  }
  for(const workload of [null,{...b.workload,unknown:1},{...b.workload,up_to_date:null}]) {
    assert.equal(h.workloadState(a,{...b,workload}),'Workload coverage incomplete');
  }
  assert.equal(h.delta(a,{...b,elapsed_ms:800}).percent,-20);
  assert.equal(h.delta({...a,elapsed_ms:0},b).percent,null);
  assert.equal(h.delta(a,{...b,status:'failed'}),null);
});
test('history renders counts, separate chart axes, controls, and safe empty/legacy states',()=>{
  const ctx=history(),a=run('1'),b=run('2',{label:'<script>bad</script>',workload:null});
  ctx.state.runs=[a,b];ctx.state.run={id:b.id};
  const html=ctx.history.shell();
  for(const text of ['history-baseline','history-kind','Duration and compilation workload','Compiled','Cache hits','Unknown','Up to date','predates cache-outcome collection','Workload coverage incomplete'])assert.ok(html.includes(text),text);
  assert.ok(!html.includes('<script>bad</script>'));
  assert.ok(!html.includes('style='),'History honors the report CSP without inline styles');
  ctx.state.runs=[];
  assert.match(ctx.history.shell(),/No recorded builds/);
});

test('an older comparison response cannot overwrite a later selection',async()=>{
  const source=readFileSync(require.resolve('../src/web/app.js'),'utf8');
  const start=source.indexOf('async function showComparison()');
  const end=source.indexOf('function drawBuildTimeline()',start);
  const controls={'#before-run':{value:'old-before'},'#after-run':{value:'old-after'},
    '#comparison-mode':{value:'captures'},'#comparison':{innerHTML:''}};
  const pending=[];
  const ctx=vm.createContext({comparisonGeneration:0,state:{view:'compare'},$:id=>controls[id],
    api:(path,query)=>new Promise(resolve=>pending.push({query,resolve})),
    metric:(label,value)=>label+':'+value,time:value=>String(value),gitLabel:()=>'',signed:String,
    notes:()=>'',table:()=>''});
  vm.runInContext(source.slice(start,end)+';globalThis.showComparison=showComparison;',ctx);
  const older=ctx.showComparison();
  controls['#before-run'].value='baseline';controls['#after-run'].value='selected';
  const newer=ctx.showComparison();
  const response=ms=>({before_elapsed_ms:ms,after_elapsed_ms:ms,before_timing_scope:'build',after_timing_scope:'build',
    delta_elapsed_ms:0,conditions_match:true,differences:[],limitations:[],units:[],headers:[]});
  pending[1].resolve(response(200));await newer;
  assert.match(controls['#comparison'].innerHTML,/Before:200/);
  pending[0].resolve(response(100));await older;
  assert.match(controls['#comparison'].innerHTML,/Before:200/);
  assert.equal(pending[1].query.before,'baseline');
  assert.equal(pending[1].query.after,'selected');
});
