/* Additive compiler work; raw traces stay on the server. */
"use strict";
const CompilerFlames = (() => {
  function layout(tree, width, row = 26) {
    const result=[];
    function visit(node,x,y,w){
      result.push({node,x,y,w,h:row-2});
      let left=x;
      for(const child of node.children||[]){const cw=node.value_us>0?w*child.value_us/node.value_us:0;visit(child,left,y+row,cw);left+=cw;}
    }
    if(tree)visit(tree,0,0,width);
    return result;
  }
  function matches(node,query){return !query||[node.name,node.detail,node.file].some(v=>String(v||"").toLowerCase().includes(query.toLowerCase()));}
  function mount(container,options={}) {
    let serial=0, disposed=false,tree=null,rects=[],selected=null,focus="",offset=0,direction="top-down";
    container.innerHTML='<div class="toolbar"><label>View <select data-flame="direction"><option value="top-down">Top down</option><option value="bottom-up">Bottom up</option></select></label> <label>Target <select data-flame="target"><option value="">All targets</option></select></label> <label>Compiler <select data-flame="compiler"><option value="">All compilers</option></select></label> <label>Highlight <input data-flame="search" type="search" placeholder="Activity, detail, source"></label> <button data-flame="back">Parent stack ↑</button> <button data-flame="reset">Reset zoom</button> <button data-flame="previous">Previous siblings</button></div><p data-flame="status" role="status"></p><canvas data-flame="canvas" tabindex="0" aria-label="Compiler flame graph. Arrow keys select frames; Enter zooms; Escape resets."></canvas><div data-flame="details"></div><div data-flame="list" aria-label="Visible compiler scopes"></div>';
    const el=k=>container.querySelector('[data-flame="'+k+'"]');
    const addOption=(select,value,label)=>{const option=document.createElement('option');option.value=value;option.textContent=label;select.append(option);};
    const inventory=state.run.project;
    const targets=[...new Set((inventory.units||[]).map(unit=>targetFor(unit)))].sort();
    for(const target of targets)addOption(el('target'),target,target);
    (inventory.compilers||[]).forEach((compiler,index)=>{if(compiler.family==='clang')addOption(el('compiler'),String(index),index+' · '+compiler.executable);});
    const locked=(inventory.units||[]).find(unit=>unit.id===options.unit);
    el('target').value=locked?targetFor(locked):options.target||"";el('compiler').value=locked?String(locked.compiler):options.compiler||"";
    if(locked){el('target').disabled=true;el('compiler').disabled=true;}
    const canvas=el('canvas'),ctx=canvas.getContext('2d');
    function choose(node){selected=node;draw();details();}
    function details(){
      const host=el('details');host.replaceChildren();if(!selected)return;
      const text=document.createElement('p');text.textContent=[selected.name,selected.detail,selected.file&&(selected.file+(selected.line?':'+selected.line:'')),time(selected.value_us/1000)+' attributed',time(selected.self_us/1000)+' recorded self',num(selected.contributing_units||0)+' contributing units'].filter(Boolean).join(' · ');host.append(text);
      if(selected.has_children){const b=document.createElement('button');b.textContent=selected.other?'Expand remaining stacks':'Focus this stack';b.onclick=()=>zoom(selected);host.append(b);}
      for(const example of selected.examples||[]){const b=document.createElement('button');b.textContent='Native scope '+example.event_id+' · '+time(example.inclusive_us/1000)+' inclusive · '+example.source;b.onclick=async()=>{await openUnit(example.unit_id,'trace');await openScope(example.event_id);};host.append(b);}
      if(selected.file){const b=document.createElement('button');b.textContent='View source';b.onclick=()=>showSource(selected.file,selected.line);host.append(b);}
    }
    function draw(){
      if(disposed||!container.isConnected)return;
      const width=Math.max(300,container.clientWidth||800);rects=layout(tree,width);
      const height=Math.max(80,...rects.map(r=>r.y+r.h+3));
      canvas.width=Math.round(width*(window.devicePixelRatio||1));canvas.height=Math.round(height*(window.devicePixelRatio||1));
      // CSS dimensions use presentation attributes, without inline style under CSP.
      canvas.setAttribute('width',canvas.width);canvas.setAttribute('height',canvas.height);
      ctx.setTransform(1,0,0,1,0,0);ctx.scale(canvas.width/width,canvas.height/height);
      ctx.clearRect(0,0,width,height);ctx.font='12px system-ui';
      const query=el('search').value;
      for(const r of rects){if(r.w<0.5)continue;const match=matches(r.node,query);let hash=0;for(const c of r.node.name||'')hash=(hash*31+c.charCodeAt(0))>>>0;
        ctx.fillStyle=query&&!match?'#cbd5e1':r.node.other?'#94a3b8':'hsl('+(hash%55+8)+' 75% 65%)';ctx.fillRect(r.x,r.y,r.w,r.h);
        if(r.node===selected){ctx.strokeStyle='#172554';ctx.lineWidth=2;ctx.strokeRect(r.x+1,r.y+1,Math.max(0,r.w-2),r.h-2);}
        if(r.w>45){ctx.save();ctx.beginPath();ctx.rect(r.x+3,r.y,r.w-6,r.h);ctx.clip();ctx.fillStyle='#17202a';ctx.fillText(r.node.detail||r.node.name,r.x+4,r.y+16);ctx.restore();}
      }
    }
    function zoom(node){focus=node.focus||"";offset=node.other?node.offset||0:0;load();}
    function accessible(){const host=el('list');host.replaceChildren();
      const select=document.createElement('select');select.setAttribute('aria-label','Select a visible recorded stack');
      rects.forEach((r,i)=>{const o=document.createElement('option');o.value=i;o.textContent=r.node.name+' '+(r.node.detail||'')+' · '+time(r.node.value_us/1000);select.append(o);});select.onchange=()=>choose(rects[Number(select.value)].node);host.append(select);
    }
    async function load(){const current=++serial;el('status').textContent='Loading recorded compiler stacks…';
      if(typeof BuildscopeTour!=='undefined'&&BuildscopeTour.active){el('status').textContent='Compiler flame graphs are unavailable for example tour data. Open a Clang profile capture to inspect recorded stacks.';return;}
      try{const response=await api('/api/compiler-flamegraph',{run:state.run.id,unit:options.unit||'',target:el('target').value,compiler:el('compiler').value,direction,focus,offset,depth:12,limit:5000});
        if(disposed||current!==serial||!container.isConnected)return;
        tree=response.tree||null;selected=tree;
        const coverage='Clang profiles '+num(response.trace_units||0)+' / '+num(response.inventory_units||0)+' selected units; '+num(response.indexed_units||0)+' indexed; thresholds '+((response.granularities_us||[]).map(value=>num(value)+' μs').join(', ')||'unrecorded')+(response.unknown_granularity_units?' ('+num(response.unknown_granularity_units)+' profiles with unrecorded threshold)':'')+'. '+(response.unprofiled_units?num(response.unprofiled_units)+' units without profiles. ':'')+(response.unsupported_units?num(response.unsupported_units)+' units have aggregate compiler profiles without temporal stacks. ':'')+(response.coverage_status==='partial'?'Coverage is incomplete; total includes observed indexed Clang work only. ':'');
        el('status').textContent=coverage+(response.status==='unavailable'?'No recorded Clang temporal stacks are available for this selection. ':'')+(response.status==='missing-index'?'Some units need indexing: '+response.index_command+'. ':'')+(response.notes||[]).join(' ')+' Total attributed work: '+time((response.total_us||0)/1000);
        draw();accessible();details();
      }catch(e){if(current===serial&&!disposed)el('status').textContent=e.message;}
    }
    canvas.onclick=e=>{const b=canvas.getBoundingClientRect();const x=(e.clientX-b.left)*(rects[0]?.w||b.width)/b.width;const h=Math.max(80,...rects.map(r=>r.y+r.h+3));const y=(e.clientY-b.top)*h/b.height;const r=rects.find(r=>x>=r.x&&x<r.x+r.w&&y>=r.y&&y<r.y+r.h);if(r)choose(r.node);};
    canvas.ondblclick=()=>{if(selected)zoom(selected);};
    canvas.onkeydown=e=>{let i=rects.findIndex(r=>r.node===selected);if(e.key==='ArrowRight'||e.key==='ArrowDown'){e.preventDefault();choose(rects[Math.min(rects.length-1,i+1)]?.node);}if(e.key==='ArrowLeft'||e.key==='ArrowUp'){e.preventDefault();choose(rects[Math.max(0,i-1)]?.node);}if(e.key==='Enter'&&selected)zoom(selected);if(e.key==='Escape'){focus='';offset=0;load();}};
    el('direction').onchange=()=>{direction=el('direction').value;focus='';offset=0;load();};
    for(const key of ['target','compiler'])el(key).onchange=()=>{focus='';offset=0;load();};
    el('search').oninput=draw;el('back').onclick=()=>{try{const path=JSON.parse(focus||'[]');path.pop();focus=JSON.stringify(path);}catch{focus='';}offset=0;load();};el('reset').onclick=()=>{focus='';offset=0;load();};el('previous').onclick=()=>{offset=0;load();};
    const observer=typeof ResizeObserver!=='undefined'?new ResizeObserver(()=>{if(!container.isConnected){disposed=true;observer.disconnect();}else draw();}):null;observer?.observe(container);
    load();return{destroy(){disposed=true;serial++;observer?.disconnect();}};
  }
  return {mount,layout,matches};
})();
if(typeof module!=='undefined')module.exports=CompilerFlames;
