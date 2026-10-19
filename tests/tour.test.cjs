// Verify the example report interface without a browser or third-party packages.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');

function examples() {
  const context = vm.createContext({structuredClone});
  vm.runInContext(readFileSync(require.resolve('../src/web/compiler-work.js'),'utf8')+';'+readFileSync(require.resolve('../src/web/tour.js'),'utf8')+';globalThis.examples = createExampleData();globalThis.tour = BuildscopeTour;',context);
  return context;
}

test('template families preserve C++ operators and expressions in unit views and the tour',()=>{
  const context=examples();
  const pairs=JSON.parse(readFileSync(require.resolve('./fixtures/template-families.json'),'utf8'));
  for(const [spelling,expected] of pairs) assert.equal(context.templateFamily(spelling),expected,spelling);
  const run={project:{units:[{id:'a',compiler:0,source_key:'a.cpp',output_key:'CMakeFiles/app.dir/a.o'},
    {id:'b',compiler:0,source_key:'b.cpp',output_key:'CMakeFiles/tests.dir/b.o'}]},
    profiles:[{unit_id:'a',family:'clang',event_count:pairs.length},{unit_id:'b',family:'clang',event_count:pairs.length}]};
  const events=unit=>pairs.map(([detail],id)=>({id,name:'InstantiateFunction',detail,duration_us:(id+1)*(unit==='a'?1000:2000),file:'header.hpp',line:id+1}));
  const result=query=>context.compilerWorkExample(run,{category:'templates',families:true,...query},events);
  const all=result({limit:200});
  assert.equal(all.total,new Set(pairs.map(pair=>pair[1])).size);
  assert.equal(all.rows.reduce((sum,row)=>sum+row.count,0),pairs.length*2);
  assert.equal(all.rows.reduce((sum,row)=>sum+row.work_ms,0),pairs.length*(pairs.length+1)/2*3);
  for(const sort of ['work','units','count','longest','name'])for(const descending of [true,false]) {
    const paged=[];
    for(let offset=0;offset<all.total;offset+=3)paged.push(...result({sort,descending,offset,limit:3}).rows);
    const field={work:'work_ms',longest:'max_ms'}[sort]||sort;
    for(let index=1;index<paged.length;index++)assert.ok(descending?paged[index-1][field]>=paged[index][field]:paged[index-1][field]<=paged[index][field]);
    assert.deepEqual(paged.map(row=>row.key).sort(),Array.from(all.rows,row=>row.key).sort());
  }
});

test('ordinary examples and the compiler profile retain separate measurement coverage',()=>{
  const {examples:data} = examples();
  assert.equal(data.runs.length,7);
  for(const row of data.runs) {
    const run = data.request('/api/run',{id:row.id});
    assert.equal(run.summary.work_ms,run.actions.reduce((sum,a)=>sum+a.end_ms-a.start_ms,0));
    assert.equal(Math.max(...run.actions.map(a=>a.end_ms)),run.elapsed_ms);
    assert.equal(run.summary.observed_units,run.project.units.length);
    for(const header of run.headers) {
      if(run.kind==='record') {
        assert.equal(header.processing_ms,null);
        assert.equal(header.measured_units,0);
      } else {
        assert.equal(header.processing_ms,header.samples_ms.reduce((sum,x)=>sum+x,0));
        assert.equal(header.measured_units,header.samples_ms.length);
      }
      const work = run.actions.filter(a=>header.units.includes(a.unit_id)).reduce((sum,a)=>sum+a.end_ms-a.start_ms,0);
      assert.equal(header.rebuilt_work_ms,work);
    }
    assert.equal(run.profiles.length,run.kind==='profile'?3:0);
  }
});

test('example details, includes, sources, and trace queries support report drill-down',()=>{
  const {examples:data} = examples();
  const run = data.profile, query={run:run.id};
  for(const unit of run.project.units) {
    const detail = data.request('/api/unit',{...query,unit:unit.id});
    assert.equal(detail.unit.id,unit.id);
    assert.equal(detail.profile.family,'clang');
    const events = data.request('/api/events',{...query,unit:unit.id});
    assert.equal(events.total,9);
    const filtered = data.request('/api/events',{...query,unit:unit.id,q:'mesh',kind:'InstantiateFunction'});
    assert.equal(filtered.total,2);
    assert.equal(filtered.events[0].name,'InstantiateFunction');
    assert.equal(data.request('/api/events',{...query,unit:unit.id,q:'absent'}).total,0);
    const page = data.request('/api/events',{...query,unit:unit.id,sort:'time',offset:1,limit:1});
    assert.equal(page.events.length,1);
    assert.equal(page.events[0].id,1);
    for(const event of events.events) {
      const scope = data.request('/api/scope',{...query,unit:unit.id,id:event.id});
      assert.equal(scope.root.id,event.id);
      for(const child of scope.children) {
        assert.equal(child.parent,event.id);
        assert.ok(child.start_us>=event.start_us);
        assert.ok(child.start_us+child.duration_us<=event.start_us+event.duration_us);
      }
    }
  }
  for(const header of run.headers) {
    const detail=data.request('/api/header',{...query,path:header.path});
    const paths=data.request('/api/include-paths',{...query,path:header.path});
    assert.equal(paths.total,detail.units.length);
    assert.ok(paths.paths.every(p=>p.chain.at(-1)===header.path));
    const source=data.request('/api/source',{...query,path:header.path});
    assert.ok(source.matches_capture);
    assert.match(source.text,/Fictional example/);
  }
});

test('comparisons preserve missing ordinary header timings and repeated sample distributions',()=>{
  const {examples:data} = examples();
  const comparison=data.request('/api/compare',{before:'example-before-1',after:'example-after-1'});
  assert.equal(comparison.delta_elapsed_ms,-2600);
  assert.ok(comparison.conditions_match);
  assert.ok(comparison.headers.every(h=>h.before_processing_ms===null&&h.delta_processing_ms===null));
  assert.equal(comparison.units[0].delta_ms,-2600);
  const series=data.request('/api/compare-series',{before_label:'Example · before',after_label:'Example · after'});
  assert.equal(series.before.count,3);
  assert.equal(series.before.median_ms,8000);
  assert.equal(series.after.median_ms,5600);
  assert.equal(series.delta_median_ms,-2400);
  assert.equal(series.before.p10_ms,7840);
  assert.equal(series.after.p90_ms,5760);
  assert.equal(data.request('/api/compare',{before:'example-profile',after:'example-after-1'}).conditions_match,false);
});

test('tour requests return independent values and unsupported routes cannot reach real data',()=>{
  const {tour} = examples();
  const run=tour.request('/api/run',{id:'example-profile'});
  run.project.units.length=0;
  run.headers[0].processing_ms=-1;
  const next=tour.request('/api/run',{id:'example-profile'});
  assert.equal(next.project.units.length,3);
  assert.equal(next.headers[0].processing_ms,6000);
  assert.throws(()=>tour.request('/api/artifact',{run:'example-profile'}),/unavailable/);
  assert.throws(()=>tour.request('/api/run',{id:'real-capture'}),/unavailable/);
});

test('global compiler work sums exact scopes and drills into units and original events',()=>{
  const {examples:data}=examples(),run=data.profile,base={run:run.id,category:'templates'};
  const result=data.request('/api/compiler-work',base);
  assert.equal(result.total,1);
  const row=result.rows[0];
  assert.equal(row.work_ms,3000);
  assert.equal(row.count,6);
  assert.equal(row.units,3);
  assert.equal(row.max_ms,1200);
  assert.equal(result.coverage[0].trace_units,3);
  const units=data.request('/api/compiler-work',{...base,entity:row.key,limit:1,offset:1});
  assert.equal(units.total,3);
  assert.equal(units.rows.length,1);
  assert.equal(units.rows[0].work_ms,1000);
  const events=data.request('/api/compiler-work',{...base,entity:row.key,breakdown:'events',unit:'example-render'});
  assert.equal(events.total,2);
  assert.equal(events.rows[0].event.name,'InstantiateFunction');
  assert.equal(events.rows[0].event.id,2);
  assert.equal(data.request('/api/scope',{run:run.id,unit:events.rows[0].unit_id,id:events.rows[0].event.id}).root.duration_us,1200000);
  const families=data.request('/api/compiler-work',{...base,families:true});
  assert.equal(families.rows[0].name,'Mesh<…>::upload()');
  assert.equal(families.rows[0].work_ms,row.work_ms);
  const members=data.request('/api/compiler-work',{...base,families:true,entity:families.rows[0].key,breakdown:'members'});
  assert.equal(members.rows[0].key,row.key);
  assert.equal(data.request('/api/compiler-work',{...base,target:'missing'}).total,0);
  assert.equal(data.request('/api/compiler-work',{...base,q:'%'}).total,0);
  const functions=data.request('/api/compiler-work',{...base,category:'functions',activity:'OptFunction'});
  assert.equal(functions.total,3);
  assert.equal(functions.rows.reduce((sum,row)=>sum+row.work_ms,0),2750);
  const evaluations=data.request('/api/compiler-work',{...base,category:'evaluations'});
  assert.equal(evaluations.rows[0].name,'');
  assert.equal(evaluations.rows[0].work_ms,360);
  assert.equal(evaluations.rows[0].count,9);
  const ordinary=data.request('/api/compiler-work',{...base,run:'example-before-1'});
  assert.equal(ordinary.total,0);
  assert.equal(ordinary.coverage[0].trace_units,0);
});
