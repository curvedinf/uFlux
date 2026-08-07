#include<cstdio>
#include<cstdlib>
int main(int argc,char**argv){
int n=argc>1?atoi(argv[1]):1000;
long count=0;
for(int y=0;y<n;y++){
double ci=2.0*y/n-1.0;
for(int x=0;x<n;x++){
double cr=2.0*x/n-1.5;
double zr=0.0,zi=0.0;
for(int i=0;i<50;i++){
double zr2=zr*zr,zi2=zi*zi;
if(zr2+zi2>4.0)break;
zi=2.0*zr*zi+ci;zr=zr2-zi2+cr;
}
if(zr*zr+zi*zi<=4.0)count++;
}}
printf("%ld\n",count);
return 0;
}
