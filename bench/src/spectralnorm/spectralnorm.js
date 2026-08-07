function eval_A(i, j) {
return 1.0 / ((i + j) * (i + j + 1) / 2 + i + 1);
}
function multiply_Av(v, n) {
const out = new Float64Array(n);
for (let i = 0; i < n; i++) {
let s = 0.0;
for (let j = 0; j < n; j++) {
s += v[j] * eval_A(i, j);
}
out[i] = s;
}
return out;
}
function multiply_Atv(v, n) {
const out = new Float64Array(n);
for (let i = 0; i < n; i++) {
let s = 0.0;
for (let j = 0; j < n; j++) {
s += v[j] * eval_A(j, i);
}
out[i] = s;
}
return out;
}
function multiply_AtAv(v, n) {
return multiply_Atv(multiply_Av(v, n), n);
}
function main() {
const n = process.argv[2] ? parseInt(process.argv[2], 10) : 5500;
let u = new Float64Array(n).fill(1.0);
let v = new Float64Array(n);
for (let k = 0; k < 10; k++) {
v = multiply_AtAv(u, n);
u = multiply_AtAv(v, n);
}
let vbv = 0.0;
let vv = 0.0;
for (let i = 0; i < n; i++) {
vbv += u[i] * v[i];
vv += v[i] * v[i];
}
process.stdout.write((Math.sqrt(vbv / vv)).toFixed(9) + "\n");
}
main();
