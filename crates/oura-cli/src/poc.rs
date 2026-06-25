//! Berendo Labs POC — live motion visualizer with raw accelerometer logging.
//!
//! Combines the 3D orientation/trajectory view from [`crate::viz`] with a JSONL
//! data logger so Will (and anyone evaluating open_oura) can see motion in real
//! time and download the raw samples for analysis. The HTTP/SSE plumbing lives
//! in [`crate::motion_server`].

use std::path::PathBuf;

use anyhow::Result;

use oura_link::ble::BleTransport;
use oura_link::OuraClient;

/// Serve the Berendo Labs POC dashboard at `127.0.0.1:port`.
pub async fn run(
    client: OuraClient<BleTransport>,
    port: u16,
    minutes: u16,
    output: PathBuf,
) -> Result<()> {
    println!("Berendo Labs POC — open_oura motion + raw data logger");
    crate::motion_server::run(
        client,
        port,
        minutes,
        INDEX_HTML,
        crate::motion_server::LogOptions {
            path: Some(output),
        },
    )
    .await
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>open_oura POC — Berendo Labs</title>
<style>
 html,body{margin:0;height:100%;background:#0b0d12;color:#cdd6f4;font:13px ui-monospace,monospace;overflow:hidden}
 canvas{display:block}
 #panel{position:fixed;top:10px;left:12px;background:#11131aee;border:1px solid #313244;border-radius:8px;padding:10px 12px;line-height:1.7;min-width:260px;max-height:calc(100vh - 24px);overflow-y:auto}
 #panel b{color:#89dceb}
 .brand{color:#cba6f7;font-size:11px;letter-spacing:.08em;text-transform:uppercase;margin-bottom:4px}
 .row{display:flex;justify-content:space-between;gap:10px;align-items:center}
 input[type=range]{width:120px}
 button{background:#1e2030;color:#cdd6f4;border:1px solid #45475a;border-radius:6px;padding:5px 10px;cursor:pointer;margin:2px 0}
 button.on{background:#a6e3a1;color:#11131a;border-color:#a6e3a1}
 button.dl{background:#313244;border-color:#89b4fa;color:#89b4fa}
 .dim{color:#7f849c}
 .warn{color:#f9e2af;font-size:12px}
 #logbox{background:#0b0d12;border:1px solid #313244;border-radius:6px;padding:6px 8px;font-size:11px;margin-top:4px}
</style></head>
<body>
<canvas id="c"></canvas>
<div id="panel">
 <div class="brand">Berendo Labs</div>
 <div><b>open_oura POC</b></div>
 <div class="dim" style="margin-bottom:6px">motion visualizer + raw logger</div>
 <div class="row"><span>stream</span><span><button id="start">Start</button> <button id="stop">Stop</button></span></div>
 <div class="row"><span class="dim">status</span><span id="status" class="dim">idle</span></div>
 <hr style="border-color:#313244"/>
 <div class="row"><span>|a|</span><span><span id="mag">--</span> g</span></div>
 <div class="row"><span>rate</span><span><span id="rate">--</span> Hz</span></div>
 <div class="row"><span>pitch / roll</span><span><span id="pr">-- / --</span>°</span></div>
 <hr style="border-color:#313244"/>
 <div><b>raw data log</b></div>
 <div id="logbox">
  <div class="row"><span class="dim">samples</span><span id="samples">0</span></div>
  <div class="row"><span class="dim">size</span><span id="bytes">0 B</span></div>
  <div class="dim" style="word-break:break-all" id="logpath">—</div>
 </div>
 <button class="dl" id="download">Download JSONL</button>
 <hr style="border-color:#313244"/>
 <div class="row"><span>smoothing</span><input id="alpha" type="range" min="1" max="40" value="8"></div>
 <div class="row"><span>damping</span><input id="damp" type="range" min="80" max="100" value="97"></div>
 <div class="row"><span>still thresh.</span><input id="zupt" type="range" min="1" max="30" value="4"></div>
 <div class="row"><span>counts/g</span><input id="cpg" type="range" min="500" max="2000" value="1024"></div>
 <div class="row"><span>path scale</span><input id="pscale" type="range" min="5" max="200" value="40"></div>
 <div class="row"><span>invert vertical ↕</span><input id="flipy" type="checkbox"></div>
 <button id="reset">Reset path</button>
 <div class="warn">trajectory drifts (no live gyro) — drag to rotate view</div>
</div>
<script>
const cv=document.getElementById('c'),ctx=cv.getContext('2d');
function resize(){cv.width=innerWidth;cv.height=innerHeight;} addEventListener('resize',resize);resize();

let az=0.6,el=0.5,drag=false,px=0,py=0;
cv.addEventListener('mousedown',e=>{drag=true;px=e.clientX;py=e.clientY});
addEventListener('mouseup',()=>drag=false);
addEventListener('mousemove',e=>{if(!drag)return;az+=(e.clientX-px)*0.01;el+=(e.clientY-py)*0.01;el=Math.max(-1.5,Math.min(1.5,el));px=e.clientX;py=e.clientY});

const $=id=>document.getElementById(id);
const set={get alpha(){return $('alpha').value/100},get damp(){return $('damp').value/100},
 get zupt(){return +$('zupt').value/100},get cpg(){return +$('cpg').value},get pscale(){return +$('pscale').value}};

const sub=(a,b)=>[a[0]-b[0],a[1]-b[1],a[2]-b[2]],add=(a,b)=>[a[0]+b[0],a[1]+b[1],a[2]+b[2]];
const sc=(a,s)=>[a[0]*s,a[1]*s,a[2]*s],dot=(a,b)=>a[0]*b[0]+a[1]*b[1]+a[2]*b[2];
const cross=(a,b)=>[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
const len=a=>Math.hypot(a[0],a[1],a[2]),norm=a=>{const l=len(a)||1;return sc(a,1/l)};

function proj(p){
 const ca=Math.cos(az),sa=Math.sin(az),ce=Math.cos(el),se=Math.sin(el);
 let x=p[0]*ca-p[2]*sa, z=p[0]*sa+p[2]*ca, y=p[1];
 let y2=y*ce-z*se;
 return [cv.width/2+x*60, cv.height/2-y2*60];
}
function line(a,b,col){const A=proj(a),B=proj(b);ctx.strokeStyle=col;ctx.beginPath();ctx.moveTo(A[0],A[1]);ctx.lineTo(B[0],B[1]);ctx.stroke();}

let G=null,vel=[0,0,0],pos=[0,0,0],still=0,trail=[],frames=0,rate=0,mag=0,pitch=0,roll=0;

function feed(d){
 const raw=[d.x,d.y,d.z];
 G=G?add(sc(G,1-set.alpha),sc(raw,set.alpha)):raw.slice();
 const up=norm(sc(G, $('flipy').checked?-1:1)); mag=len(raw)/set.cpg;
 pitch=Math.atan2(up[2],up[1])*180/Math.PI; roll=Math.atan2(up[0],up[1])*180/Math.PI;
 const r0=Math.abs(up[1])<0.9?[0,1,0]:[1,0,0];
 const right=norm(cross(up,r0)), fwd=cross(right,up);
 const linS=sc(sub(raw,G),1/set.cpg);
 const linW=[dot(linS,right),dot(linS,up),dot(linS,fwd)];
 const dt=0.02;
 if(len(linW)<set.zupt){if(++still>8)vel=[0,0,0];}else still=0;
 vel=sc(add(vel,sc(linW,9.81*dt)),set.damp);
 pos=add(pos,sc(vel,dt*set.pscale));
 trail.push(pos.slice()); if(trail.length>800)trail.shift();
 frames++;
}

function draw(){
 requestAnimationFrame(draw);
 ctx.clearRect(0,0,cv.width,cv.height);
 for(let i=-5;i<=5;i++){line([i,0,-5],[i,0,5],'#1e2030');line([-5,0,i],[5,0,i],'#1e2030');}
 line([0,0,0],[1.5,0,0],'#f38ba8');line([0,0,0],[0,1.5,0],'#a6e3a1');line([0,0,0],[0,0,1.5],'#89b4fa');
 ctx.strokeStyle='#f38ba8';ctx.beginPath();
 for(let i=0;i<trail.length;i++){const P=proj(trail[i]);i?ctx.lineTo(P[0],P[1]):ctx.moveTo(P[0],P[1]);}ctx.stroke();
 if(G){const n=norm(G);let t1=norm(cross(n,Math.abs(n[0])<0.9?[1,0,0]:[0,0,1]));let t2=cross(n,t1);
  ctx.strokeStyle='#89b4fa';ctx.beginPath();
  for(let k=0;k<=32;k++){const a=k/32*Math.PI*2;const pt=add(pos,add(sc(t1,0.5*Math.cos(a)),sc(t2,0.5*Math.sin(a))));const P=proj(pt);k?ctx.lineTo(P[0],P[1]):ctx.moveTo(P[0],P[1]);}ctx.stroke();}
 $('mag').textContent=mag.toFixed(2);$('rate').textContent=rate.toFixed(0);
 $('pr').textContent=pitch.toFixed(0)+' / '+roll.toFixed(0);
}
draw();
setInterval(()=>{rate=frames;frames=0;},1000);

function fmtBytes(n){if(n<1024)return n+' B';if(n<1048576)return (n/1024).toFixed(1)+' KB';return (n/1048576).toFixed(1)+' MB';}
async function refreshStats(){
 try{const r=await fetch('/stats');const s=await r.json();
  $('samples').textContent=s.samples.toLocaleString();
  $('bytes').textContent=fmtBytes(s.bytes);
  $('logpath').textContent=s.path||'—';
 }catch(e){}
}
setInterval(refreshStats,1000);refreshStats();

const es=new EventSource('/stream');
es.onmessage=e=>feed(JSON.parse(e.data));

$('reset').onclick=()=>{trail=[];pos=[0,0,0];vel=[0,0,0];};
const H={headers:{'X-Oura-Viz':'1'}};
$('start').onclick=async()=>{await fetch('/start',H);$('start').classList.add('on');$('stop').classList.remove('on');$('status').textContent='streaming';};
$('stop').onclick=async()=>{await fetch('/stop',H);$('start').classList.remove('on');$('stop').classList.add('on');$('status').textContent='stopped';refreshStats();};
$('download').onclick=()=>{location.href='/download';};
addEventListener('pagehide',()=>{try{fetch('/stop',{headers:H.headers,keepalive:true});}catch(e){}});
</script>
</body></html>"##;
