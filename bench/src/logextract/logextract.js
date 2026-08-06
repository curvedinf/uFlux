"use strict";
const fs=require("fs");
const readline=require("readline");
const inputFile=process.argv[2]||"bench/data/access.log";
let tl=0,tb=0,s2=0,s3=0,s4=0,s5=0;
const hr=new Array(24).fill(0);
const ic=new Map();
const rl=readline.createInterface({input:fs.createReadStream(inputFile),crlfDelay:Infinity});
rl.on("line",(line)=>{
 const t=line.split(" ");
 if(t.length<10)return;
 const ip=t[0];
 const hour=parseInt(t[3].split(":")[1],10);
 const status=parseInt(t[8],10);
 const nb=parseInt(t[9],10);
 tl++;tb+=nb;
 const c=(status/100)|0;
 if(c===2)s2++;else if(c===3)s3++;else if(c===4)s4++;else if(c===5)s5++;
 if(hour>=0&&hour<24)hr[hour]++;
 ic.set(ip,(ic.get(ip)||0)+1);
});
rl.on("close",()=>{
 const top=[...ic.entries()].sort((a,b)=>b[1]-a[1]||(a[0]<b[0]?-1:a[0]>b[0]?1:0)).slice(0,10);
 const o=["=== LOG EXTRACTION RESULTS ===",`total_lines: ${tl}`,`total_bytes: ${tb}`,`status_2xx: ${s2}`,`status_3xx: ${s3}`,`status_4xx: ${s4}`,`status_5xx: ${s5}`];
 for(let h=0;h<24;h++)o.push(`hour_${String(h).padStart(2,"0")}: ${hr[h]}`);
 for(let i=0;i<top.length;i++)o.push(`top_ip_${i+1}: ${top[i][0]} (${top[i][1]})`);
 process.stdout.write(o.join("\n")+"\n");
});
