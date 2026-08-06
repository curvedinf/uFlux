"use strict";
const fs=require("fs");
const readline=require("readline");
const inputFile=process.argv[2]||"bench/data/sales.csv";
let tr=0,rev=0,units=0;
const rr=new Map(),rp=new Map(),rc=new Map();
const rl=readline.createInterface({input:fs.createReadStream(inputFile),crlfDelay:Infinity});
let header=true;
rl.on("line",(line)=>{
 if(header){header=false;return;}
 const c=line.split(",");
 if(c.length<12)return;
 tr++;
 const u=parseInt(c[7],10);
 const t=parseFloat(c[10]);
 rev+=t;
 units+=u;
 const region=c[2],country=c[3],product=c[4];
 rr.set(region,(rr.get(region)||0)+t);
 rp.set(product,(rp.get(product)||0)+t);
 rc.set(country,(rc.get(country)||0)+t);
});
rl.on("close",()=>{
 const avg=tr>0?rev/tr:0;
 const srt=(a,b)=>b[1]-a[1]||(a[0]<b[0]?-1:a[0]>b[0]?1:0);
 const rg=[...rr.entries()].sort(srt);
 const pr=[...rp.entries()].sort(srt).slice(0,5);
 const ct=[...rc.entries()].sort(srt).slice(0,3);
 const o=["=== DATA ANALYTICS RESULTS ===",`total_rows: ${tr}`,`total_revenue: ${rev.toFixed(2)}`,`avg_order: ${avg.toFixed(2)}`,`total_units: ${units}`];
 for(let i=0;i<rg.length;i++)o.push(`region_${i+1}: ${rg[i][0]} $${rg[i][1].toFixed(2)}`);
 for(let i=0;i<pr.length;i++)o.push(`product_${i+1}: ${pr[i][0]} $${pr[i][1].toFixed(2)}`);
 for(let i=0;i<ct.length;i++)o.push(`country_${i+1}: ${ct[i][0]} $${ct[i][1].toFixed(2)}`);
 process.stdout.write(o.join("\n")+"\n");
});
