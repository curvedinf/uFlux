#include<cmath>
#include<cstdio>
#include<cstdlib>
inline double eval_A(int i,int j){return 1.0/((double)((long long)(i+j)*(i+j+1)/2+i+1));}
void multiply_Av(const double*v,double*out,int n){for(int i=0;i<n;i++){double s=0.0;for(int j=0;j<n;j++)s+=v[j]*eval_A(i,j);out[i]=s;}}
void multiply_Atv(const double*v,double*out,int n){for(int i=0;i<n;i++){double s=0.0;for(int j=0;j<n;j++)s+=v[j]*eval_A(j,i);out[i]=s;}}
void multiply_AtAv(const double*v,double*out,int n){double*tmp=new double[n];multiply_Av(v,tmp,n);multiply_Atv(tmp,out,n);delete[] tmp;}
int main(int argc,char*argv[]){int n=argc>1?std::atoi(argv[1]):5500;double*u=new double[n];double*v=new double[n];for(int i=0;i<n;i++){u[i]=1.0;v[i]=0.0;}for(int k=0;k<10;k++){multiply_AtAv(u,v,n);multiply_AtAv(v,u,n);}double vBv=0.0,vv=0.0;for(int i=0;i<n;i++){vBv+=u[i]*v[i];vv+=v[i]*v[i];}std::printf("%.9f\n",std::sqrt(vBv/vv));delete[] u;delete[] v;return 0;}
