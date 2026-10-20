const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const source=fs.readFileSync('src/web/header-hotspots.js','utf8');
function setup(response){
  const content={innerHTML:'',isConnected:true};const controls=new Map();
  for(const id of ['#header-hotspots-content','#hotspot-filter','#hotspot-target','#hotspot-days','#hotspot-prev','#hotspot-next'])controls.set(id,id==='#header-hotspots-content'?content:{});
  let query;
  const context=vm.createContext({state:{run:{id:'run<1>',project:{units:[{target:'app"x'}]}}},
    $:id=>controls.get(id),targetFor:u=>u.target,esc:s=>String(s??'').replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('"','&quot;'),
    num:String,time:v=>v==null?'Unavailable':v+' ms',date:String,
    notes:a=>a.join('|'),table:(columns,rows)=>columns.map(c=>c.label).join(',')+rows.map(r=>columns.map(c=>c.render(r)).join(',')).join(''),
    metric:(a,b,c)=>[a,b,c].join(' '),modal:html=>{content.innerHTML=html;},toast:()=>{},
    api:async(path,q)=>{query={path,...q};return response;}});
  vm.runInContext(source+'\nglobalThis.moduleUnderTest=HeaderHotspots;',context);
  return {module:context.moduleUnderTest,context,content,controls,query:()=>query};
}
test('shell offers escaped target and 90 day default with independent controls',()=>{
  const {module}=setup({});const html=module.shell();
  assert.match(html,/value="90" selected/);assert.match(html,/hotspot-filter/);
  assert.match(html,/app&quot;x/);assert.doesNotMatch(html,/id="filter"/);
});
test('missing analysis shows explicit command and never reads git',async()=>{
  const s=setup({available:false,notes:['No saved history']});await s.module.load();
  assert.match(s.content.innerHTML,/analyze-history run&lt;1>/);
  assert.equal(s.query().path,'/api/header-hotspots');assert.equal(s.query().days,90);
});
test('partial saved windows disclose unavailable priority and unit coverage',async()=>{
  const s=setup({available:true,complete:false,window_truncated:true,anchor:'abc',since_ms:1,until_ms:2,total:1,rows:[{key:'a<b.hpp',path:'/a',editing_commits:2,dependent_units:3,measured_units:1,work_ms:125,processing_ms:null,priority:null}]});
  await s.module.load();assert.match(s.content.innerHTML,/less than the selected window/);
  assert.match(s.content.innerHTML,/1 \/ 3 units/);assert.match(s.content.innerHTML,/Unavailable/);
  assert.match(s.content.innerHTML,/a&lt;b.hpp/);assert.equal(s.query().limit,100);
});
test('detail requests evidence only for selected path and escapes commits',async()=>{
  const s=setup({available:true,anchor:'abc',rows:[{key:'a.hpp',editing_commits:1,dependent_units:1,measured_units:1,work_ms:5,priority:5,edits:[{commit:'bad<script>',committed_ms:1}],renames:[['old<','new']]}]});
  await s.module.detail('/a.hpp');assert.equal(s.query().path,'/a.hpp');assert.equal(s.query().limit,1);
  assert.match(s.content.innerHTML,/bad&lt;script>/);assert.match(s.content.innerHTML,/old&lt;/);
});

test('late detail response cannot replace a newer selection or another modal',async()=>{
  const s=setup({});const pending=[];
  s.context.api=()=>new Promise(resolve=>pending.push(resolve));
  const first=s.module.detail('/first');const second=s.module.detail('/second');
  pending[1]({anchor:'abc',rows:[{key:'second.hpp',path:'/second',edits:[],renames:[]}]});await second;
  assert.match(s.content.innerHTML,/data-action="header" data-id="\/second"/);
  pending[0]({anchor:'abc',rows:[{key:'first.hpp',edits:[],renames:[]}]});await first;
  assert.match(s.content.innerHTML,/second.hpp/);assert.doesNotMatch(s.content.innerHTML,/first.hpp/);
  const third=s.module.detail('/third');s.context.state.detail={type:'unit',id:'unit'};
  pending[2]({anchor:'abc',rows:[{key:'third.hpp',edits:[],renames:[]}]});await third;
  assert.doesNotMatch(s.content.innerHTML,/third.hpp/);
});
