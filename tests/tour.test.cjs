// Verify the example report interface without a browser or third-party packages.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');

function examples() {
  const context = vm.createContext({structuredClone});
  vm.runInContext(readFileSync(require.resolve('../src/web/tour.js'),'utf8')+';globalThis.examples = createExampleData();globalThis.tour = BuildscopeTour;',context);
  return context;
}

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
    assert.equal(events.total,4);
    const filtered = data.request('/api/events',{...query,unit:unit.id,q:'mesh',kind:'InstantiateFunction'});
    assert.equal(filtered.total,1);
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
