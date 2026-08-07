use std::env;
fn main(){
let n:usize=env::args().nth(1).and_then(|s|s.parse().ok()).unwrap_or(1000);
let mut count:i64=0;
for y in 0..n{
let ci=2.0*y as f64/n as f64-1.0;
for x in 0..n{
let cr=2.0*x as f64/n as f64-1.5;
let mut zr=0.0f64;let mut zi=0.0f64;
for _ in 0..50{
let zr2=zr*zr;let zi2=zi*zi;
if zr2+zi2>4.0{break;}
zi=2.0*zr*zi+ci;zr=zr2-zi2+cr;
}
if zr*zr+zi*zi<=4.0{count+=1;}
}}
println!("{}",count);
}
