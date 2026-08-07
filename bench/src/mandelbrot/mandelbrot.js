const n=parseInt(process.argv[2])||1000;
let count=0;
for(let y=0;y<n;y++){
const ci=2.0*y/n-1.0;
for(let x=0;x<n;x++){
const cr=2.0*x/n-1.5;
let zr=0.0,zi=0.0;
for(let i=0;i<50;i++){
const zr2=zr*zr,zi2=zi*zi;
if(zr2+zi2>4.0)break;
zi=2.0*zr*zi+ci;zr=zr2-zi2+cr;
}
if(!((zr*zr+zi*zi)>4.0))count++;
}}
console.log(count);
