// ---------------- C prelude ----------------
pub const PRELUDE: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <unistd.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sched.h>
#include <sys/syscall.h>
#ifdef _WIN32
#include <process.h>
#else
#include <sys/wait.h>
#include <fnmatch.h>
#endif

enum { T_INT=0, T_FLOAT=1, T_PTR=2 };
/* Slim 16-byte cell: pointer payloads live in i (cast at use sites), float
   payloads are the double bit pattern in i. uf_f()/uf_fromf() convert.
   (This keeps a whole cell in two GP registers and lets cc -O2 scalarize
   fused basic blocks.) */
typedef struct { int tag; int64_t i; } Cell;
/* handle tags (TYPEOF results): 0..4 are the scalar type ids, handles start at 5 */
enum { HT_ARR=5, HT_TENSOR=6, HT_DYN=7, HT_MAP=8, HT_RING=9, HT_ATOM=10, HT_STR=11, HT_BUF=12, HT_OBJ=13 };
typedef struct { uint64_t tag; uint64_t len; uint64_t esz; char data[]; } Hdr;
/* container structs; only the {tag,len,esz} prefix is shared with Hdr, so any
   generic access to elements must go through uf_data() */
typedef struct { uint64_t tag; uint64_t len; uint64_t esz; uint64_t cap; Cell data[]; } Dyn;
typedef struct { uint64_t tag; uint64_t len; uint64_t cap; Cell* keys; Cell* vals; unsigned char* st; } Map;
typedef struct { uint64_t tag; uint64_t len; uint64_t cap; Cell* buf; uint64_t head; uint64_t tail; pthread_mutex_t mu; pthread_cond_t notfull; pthread_cond_t notempty; int closed; } Ring;
typedef struct { uint64_t tag; uint64_t len; _Atomic int64_t v; } Atom;
static char* uf_data(Hdr*a){ return a->tag==HT_DYN ? (char*)((Dyn*)a)->data : (char*)a->data; }

/* per-task execution context: each weave task runs with its own stacks */
typedef struct { Cell* ds; long sp; long dcap; const void** cs; long csp; long ccap; } Ctx;
static void die(const char*m){ fprintf(stderr,"uflux: %s\n",m); exit(1); }
static Ctx* ctx_new(long dcap,long ccap){ Ctx*c=(Ctx*)malloc(sizeof(Ctx)); c->ds=(Cell*)malloc(dcap*sizeof(Cell)); c->cs=(const void**)malloc(ccap*sizeof(void*)); c->sp=0; c->csp=0; c->dcap=dcap; c->ccap=ccap; return c; }
static Cell main_ds[1<<20]; static const void* main_cs[1<<16];
static Ctx main_cx_store = { main_ds, 0, 1<<20, main_cs, 0, 1<<16 };
static Ctx* main_cx = &main_cx_store;
int64_t uf_argc=0; void* uf_argv=0; /* program args, reachable via EXTERN "uf_argc"/"uf_argv" + LOADX */

static void pushc(Ctx*cx,Cell c){ if(cx->sp>=cx->dcap)die("stack overflow"); cx->ds[cx->sp++]=c; }
/* pure Cell constructors/arith: shared by the op helpers and by the fused
   basic-block codegen (which keeps values in C locals instead of the ds) */
static inline Cell uf_mki(int64_t v){ Cell c; c.tag=T_INT; c.i=v; return c; }
static inline Cell uf_mkp(void* v){ Cell c; c.tag=T_PTR; c.i=(int64_t)v; return c; }
static inline double uf_fbits(int64_t i){ union{int64_t i;double f;}u;u.i=i;return u.f; }
static inline int64_t uf_ibits(double f){ union{int64_t i;double f;}u;u.f=f;return u.i; }
/* numeric value of a cell as a double (tag-aware, for mixed int/float arith) */
static inline double uf_f(Cell c){ return c.tag==T_FLOAT?uf_fbits(c.i):(double)c.i; }
static inline Cell uf_mkf(double v){ Cell c; c.tag=T_FLOAT; c.i=uf_ibits(v); return c; }
static inline Cell uf_fromf(double v){ return uf_mkf(v); }
/* truthiness as JZ historically defined it: (int64_t)value == 0 */
static inline int uf_zero(Cell c){ return c.tag==T_FLOAT?(int64_t)uf_fbits(c.i)==0:c.i==0; }
static inline Cell uf_cadd(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)+uf_f(b)); return uf_mki(a.i+b.i); }
static inline Cell uf_csub(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)-uf_f(b)); return uf_mki(a.i-b.i); }
static inline Cell uf_cmul(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)*uf_f(b)); return uf_mki(a.i*b.i); }
static inline Cell uf_cand(Cell a,Cell b){ return uf_mki(a.i&b.i); }
static inline Cell uf_cshr(Cell a){ return uf_mki((int64_t)((uint64_t)a.i>>1)); }
static inline Cell uf_cinc(Cell a){ if(a.tag==T_FLOAT)return uf_fromf(uf_f(a)+1.0); return uf_mki(a.i+1); }
static inline Cell uf_cdec(Cell a){ if(a.tag==T_FLOAT)return uf_fromf(uf_f(a)-1.0); return uf_mki(a.i-1); }
static inline int uf_ceq(Cell a,Cell b){ return (a.tag==T_FLOAT||b.tag==T_FLOAT)?uf_f(a)==uf_f(b):a.i==b.i; }
static inline Cell uf_cidx(Cell h,int64_t ix){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("IDX out of bounds"); char*dt=uf_data(a); if(a->esz==8)return uf_mki(((int64_t*)dt)[ix]); if(a->esz==sizeof(Cell))return ((Cell*)dt)[ix]; return uf_mki((int64_t)((uint8_t*)dt)[ix]); }
static inline void uf_cseti(Cell h,int64_t ix,Cell v){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("SETI out of bounds"); char*dt=uf_data(a); if(a->esz==8)((int64_t*)dt)[ix]=v.i; else if(a->esz==sizeof(Cell))((Cell*)dt)[ix]=v; else ((uint8_t*)dt)[ix]=(uint8_t)v.i; }
static void pushi(Ctx*cx,int64_t v){ pushc(cx,uf_mki(v)); }
static void pushf(Ctx*cx,double v){ pushc(cx,uf_mkf(v)); }
static void pushp(Ctx*cx,void* v){ pushc(cx,uf_mkp(v)); }
static Cell pop(Ctx*cx){ if(cx->sp<=0) die("stack underflow"); return cx->ds[--cx->sp]; }
static void op_nop(Ctx*cx){ (void)cx; }

static void op_dup(Ctx*cx){ if(cx->sp<1)die("stack underflow"); pushc(cx,cx->ds[cx->sp-1]); }
static void op_ovr(Ctx*cx){ if(cx->sp<2)die("stack underflow"); pushc(cx,cx->ds[cx->sp-2]); }
static void op_drp(Ctx*cx){ (void)pop(cx); }
static void op_swp(Ctx*cx){ Cell t=cx->ds[cx->sp-1]; cx->ds[cx->sp-1]=cx->ds[cx->sp-2]; cx->ds[cx->sp-2]=t; }
static void op_pick(Ctx*cx){ int64_t n=pop(cx).i; if(n<0||n>=cx->sp)die("PICK out of range"); pushc(cx,cx->ds[cx->sp-1-n]); }

static void op_add(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cadd(a,b)); }
static void op_sub(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_csub(a,b)); }
static void op_mul(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cmul(a,b)); }
static void op_and(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cand(a,b)); }
static void op_shr(Ctx*cx){ pushc(cx,uf_cshr(pop(cx))); }
static void op_inc(Ctx*cx){ pushc(cx,uf_cinc(pop(cx))); }
static void op_dec(Ctx*cx){ pushc(cx,uf_cdec(pop(cx))); }

static void* uf_alloc(size_t sz,int align){ void*p=NULL; if(align>0){ if(posix_memalign(&p,(size_t)align,sz?sz:1))die("alloc failed"); } else { p=malloc(sz?sz:1); } if(!p)die("out of memory"); return p; }
static void op_arrn(Ctx*cx,uint64_t tag,int align){ int64_t len=pop(cx).i, ty=pop(cx).i; int64_t esz=(ty==3)?1:8; if(len<0)die("negative length"); Hdr*h=(Hdr*)uf_alloc(sizeof(Hdr)+(size_t)len*(size_t)esz,align); h->tag=tag; h->len=(uint64_t)len; h->esz=(uint64_t)esz; memset(h->data,0,(size_t)len*(size_t)esz); pushp(cx,h); }
static void op_arr(Ctx*cx){ op_arrn(cx,HT_ARR,0); }
static void op_tensor(Ctx*cx){ op_arrn(cx,HT_TENSOR,64); }
static void op_idx(Ctx*cx){ int64_t ix=pop(cx).i; Cell h=pop(cx); pushc(cx,uf_cidx(h,ix)); }
static void op_seti(Ctx*cx){ Cell v=pop(cx); int64_t ix=pop(cx).i; Cell h=pop(cx); uf_cseti(h,ix,v); }
static void op_clone(Ctx*cx){ Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); size_t sz=sizeof(Hdr)+(size_t)a->len*a->esz; void*n=uf_alloc(sz,0); memcpy(n,a,sz); memcpy(uf_data((Hdr*)n),uf_data(a),(size_t)a->len*a->esz); pushp(cx,n); }
static void op_cast(Ctx*cx){ Cell id=pop(cx); Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); int64_t tk=(a->tag==HT_OBJ)?1000+(int64_t)a->len:(int64_t)a->tag; if(tk!=id.i)die("CAST: type mismatch"); pushc(cx,h); }

/* OBJ: size in low 32 bits of the operand, struct id above; the object carries
   a Hdr (tag HT_OBJ, len=struct id) so TYPEOF/FIELDS/SEND/CAST can see it.
   GET/SET offsets are relative to the object data (after the header). */
static void op_obj(Ctx*cx){ int64_t v=pop(cx).i; int64_t sz=v&0xffffffffLL; int64_t sid=v>>32; if(sz<=0)sz=8; Hdr*h=(Hdr*)uf_alloc(sizeof(Hdr)+(size_t)sz,0); h->tag=HT_OBJ; h->len=(uint64_t)sid; h->esz=8; memset(h->data,0,(size_t)sz); pushp(cx,h); }
static void op_get(Ctx*cx){ int64_t o=pop(cx).i; Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); pushi(cx,*(int64_t*)(a->data+o)); }
/* SET: handle offset value ->  (same convention as SETI; GET: handle offset -> v) */
static void op_set(Ctx*cx){ Cell v=pop(cx); int64_t o=pop(cx).i; Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); *(int64_t*)(a->data+o)=v.i; }

static void op_buf(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative BUF size"); void*p=uf_alloc((size_t)sz,0); memset(p,0,(size_t)sz); pushp(cx,p); }
static void op_bufcopy(Ctx*cx){ int64_t n=pop(cx).i; Cell s=pop(cx),d=pop(cx); if(n>0)memmove(((void*)d.i),((void*)s.i),(size_t)n); }
static void op_loadx(Ctx*cx){ Cell a=pop(cx); pushi(cx,*(int64_t*)((void*)a.i)); }
static void op_storex(Ctx*cx){ Cell a=pop(cx); Cell v=pop(cx); *(int64_t*)((void*)a.i)=v.i; }
static void op_malloc(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative MALLOC size"); void*p=malloc((size_t)sz?sz:1); if(!p)die("out of memory"); pushp(cx,p); }
static void op_free(Ctx*cx){ Cell p=pop(cx); free(((void*)p.i)); }
static void op_sizeof(Ctx*cx){ int64_t ty=pop(cx).i; pushi(cx,ty==3?1:8); }

static void op_cat(Ctx*cx){ Cell b=pop(cx),a=pop(cx); size_t la=strlen((char*)((void*)a.i)),lb=strlen((char*)((void*)b.i)); char*r=(char*)uf_alloc(la+lb+1,0); memcpy(r,((void*)a.i),la); memcpy(r+la,((void*)b.i),lb+1); pushp(cx,r); }
static int uf_count(const char*f){ int c=0; for(;f&&*f;f++){ if(*f=='%'){ if(f[1]=='%'){ f++; } else { c++; if(f[1]=='*')c++; } } } return c; }
static char* uf_fmt(const char*f,Cell*a,int n){
  size_t cap=256,bi=0; char*buf=(char*)uf_alloc(cap,0); int ai=0;
  for(const char*p=f;*p;){
    if(*p!='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]=*p++; continue; }
    if(p[1]=='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]='%'; p+=2; continue; }
    char d[32]; int di=0; d[di++]='%'; p++;
    while(*p&&strchr("-+ #0",*p)) d[di++]=*p++;
    while(*p&&(isdigit((unsigned char)*p)||*p=='.')) d[di++]=*p++;
    while(*p&&strchr("hlLjzt",*p)) p++;
    char conv=*p?*p++:'d';
    if(ai>=n) die("FMT: not enough args");
    Cell ar=a[ai++];
    char tmp[128]; int tl=0;
    d[di]=0;
    switch(conv){
      case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
        { size_t l=strlen(d); d[l]='l'; d[l+1]='l'; d[l+2]=conv; d[l+3]=0; }
        tl=snprintf(tmp,sizeof(tmp),d,(unsigned long long)ar.i); break; }
      case 'c': tl=snprintf(tmp,sizeof(tmp),d,(int)ar.i); break;
      case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
        size_t l=strlen(d); d[l]=conv; d[l+1]=0;
        tl=snprintf(tmp,sizeof(tmp),d,uf_f(ar)); break; }
      case 's': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,(char*)((void*)ar.i)); break; }
      case 'p': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,((void*)ar.i)); break; }
      default: die("FMT: unsupported directive");
    }
    if(tl<0) die("FMT failed");
    while(bi+(size_t)tl+1>cap){ cap*=2; buf=(char*)realloc(buf,cap); }
    memcpy(buf+bi,tmp,(size_t)tl); bi+=(size_t)tl;
  }
  buf[bi]=0; return buf;
}
static void op_fmt(Ctx*cx){ Cell f=pop(cx); int n=uf_count((char*)((void*)f.i)); Cell args[16]; if(n>16)die("FMT: too many args"); for(int k=n-1;k>=0;k--) args[k]=pop(cx); pushp(cx,uf_fmt((char*)((void*)f.i),args,n)); }
/* PRINT: fmt args.. -> n ; fmt is ON TOP with args below it (deepest first) */
static void op_print(Ctx*cx){ Cell f=pop(cx); int n=uf_count((char*)((void*)f.i)); Cell args[16]; if(n>16)die("PRINT: too many args"); for(int k=n-1;k>=0;k--) args[k]=pop(cx); char*s=uf_fmt((char*)((void*)f.i),args,n); int r=printf("%s",s); pushi(cx,(int64_t)r); }
/* SCAN: fmt -> values.. count ; per conversion fscanf(stdin,..):
   %d/%i -> i64 (via "%lld"), %f/%e/%g -> f64 (via "%lf" into double),
   %s -> freshly allocated string handle. Literal text in fmt unsupported. */
static void op_scan(Ctx*cx){
  Cell f=pop(cx); const char*p=(const char*)((void*)f.i); int n=0;
  for(;*p;p++){
    if(*p=='%'){
      if(p[1]=='%'){ p++; continue; }
      p++;
      while(*p&&strchr("-+ #0",*p)) p++;
      while(*p&&(isdigit((unsigned char)*p)||*p=='.')) p++;
      while(*p&&strchr("hlLjzt",*p)) p++;
      char conv=*p?*p:'\0';
      switch(conv){
        case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
          long long v; char d[8]; d[0]='%'; d[1]='l'; d[2]='l'; d[3]=conv; d[4]=0;
          if(fscanf(stdin,d,&v)!=1) die("SCAN: input error"); pushi(cx,(int64_t)v); n++; break; }
        case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
          double v; char d[8]; d[0]='%'; d[1]='l'; d[2]='f'; d[3]=0;
          if(fscanf(stdin,d,&v)!=1) die("SCAN: input error"); pushf(cx,v); n++; break; }
        case 's': {
          char*b=(char*)uf_alloc(1<<16,0);
          if(fscanf(stdin,"%65535s",b)!=1) die("SCAN: input error"); pushp(cx,b); n++; break; }
        default: die("SCAN: unsupported directive");
      }
    } else if(isspace((unsigned char)*p)) {
      continue; /* fmt whitespace matches optional input whitespace implicitly */
    } else {
      die("SCAN: literal text in format unsupported");
    }
  }
  pushi(cx,(int64_t)n);
}
static int uf_vargc(Ctx*cx){ for(int t=0;t<cx->sp;t++){ Cell fc=cx->ds[cx->sp-1-t]; if(fc.tag==T_PTR&&((void*)fc.i)&&uf_count((char*)((void*)fc.i))==t) return t; } die("vararg call: format string not found"); return 0; }

/* ---- v9: op-native datastructures ---- */
/* DYN: growable Cell vector; IDX/SETI/LEN work via the shared header prefix */
static void op_list(Ctx*cx){ Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+8*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=8; pushp(cx,d); }
static void op_append(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("APPEND: not a list"); if(d->len>=d->cap){ d->cap*=2; d=(Dyn*)realloc(d,sizeof(Dyn)+d->cap*sizeof(Cell)); if(!d)die("out of memory"); } d->data[d->len++]=v; pushp(cx,d); }
static void op_lpop(Ctx*cx){ Cell h=pop(cx); Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("POP: not a list"); if(d->len==0)die("POP: empty list"); pushc(cx,d->data[--d->len]); }

/* MAP: open addressing, FNV-1a, tombstones, grow at 70% load.
   Keys are cells: ints compare by value, pointers compare as C strings. */
static uint64_t uf_fnv(const void*p,size_t n){ const unsigned char*s=(const unsigned char*)p; uint64_t h=1469598103934665603ULL; for(size_t i=0;i<n;i++){ h^=s[i]; h*=1099511628211ULL; } return h; }
static uint64_t map_hash(Cell k){ if(k.tag==T_PTR&&((void*)k.i)) return uf_fnv(((void*)k.i),strlen((char*)((void*)k.i))); return uf_fnv(&k.i,8); }
static int map_keyeq(Cell a,Cell b){ if(a.tag==T_PTR&&b.tag==T_PTR&&((void*)a.i)&&((void*)b.i)) return strcmp((char*)((void*)a.i),(char*)((void*)b.i))==0; return a.i==b.i; }
static void map_put_raw(Map*m,Cell k,Cell v){
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]!=1){ m->st[i]=1; m->keys[i]=k; m->vals[i]=v; m->len++; return; } if(map_keyeq(m->keys[i],k)){ m->vals[i]=v; return; } i=(i+1)%m->cap; }
}
static void map_grow(Map*m){
  uint64_t ncap=m->cap*2; Cell*ok=m->keys,*ov=m->vals; unsigned char*os=m->st; uint64_t ocap=m->cap;
  m->cap=ncap; m->keys=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->st=(unsigned char*)calloc(ncap,1); m->len=0;
  for(uint64_t i=0;i<ocap;i++) if(os[i]==1) map_put_raw(m,ok[i],ov[i]);
  free(ok); free(ov); free(os);
}
static void op_dict(Ctx*cx){ Map*m=(Map*)uf_alloc(sizeof(Map),0); m->tag=HT_MAP; m->len=0; m->cap=16; m->keys=(Cell*)uf_alloc(16*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(16*sizeof(Cell),0); m->st=(unsigned char*)calloc(16,1); pushp(cx,m); }
/* DPUT: h k v ->  (v on top) */
static void op_dput(Ctx*cx){ Cell v=pop(cx),k=pop(cx),h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DPUT: not a dict"); if((m->len+1)*10>=m->cap*7) map_grow(m); map_put_raw(m,k,v); }
/* DGET: h k -> v found (two cells; found flag on top) */
static void op_dget(Ctx*cx){ Cell k=pop(cx),h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DGET: not a dict"); uint64_t i=map_hash(k)%m->cap; for(;;){ if(m->st[i]==0){ pushi(cx,0); pushi(cx,0); return; } if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ pushc(cx,m->vals[i]); pushi(cx,1); return; } i=(i+1)%m->cap; } }
static void op_ddel(Ctx*cx){ Cell k=pop(cx),h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DDEL: not a dict"); uint64_t i=map_hash(k)%m->cap; for(;;){ if(m->st[i]==0) return; if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ m->st[i]=2; m->len--; return; } i=(i+1)%m->cap; } }
static void op_dcount(Ctx*cx){ Cell h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DCOUNT: not a dict"); pushi(cx,(int64_t)m->len); }
static void op_dkeys(Ctx*cx){ Cell h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DKEYS: not a dict"); Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+(m->len?m->len:1)*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=m->len?m->len:1; for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1) d->data[d->len++]=m->keys[i]; pushp(cx,d); }

/* RING/CHAN: bounded MPSC ring buffer with blocking ENQ/DEQ */
static void ring_enq(Ring*r,Cell v){ pthread_mutex_lock(&r->mu); while(r->len>=r->cap&&!r->closed) pthread_cond_wait(&r->notfull,&r->mu); if(r->closed){ pthread_mutex_unlock(&r->mu); die("ENQ: chan closed"); } r->buf[r->tail]=v; r->tail=(r->tail+1)%r->cap; r->len++; pthread_cond_signal(&r->notempty); pthread_mutex_unlock(&r->mu); }
static void ring_close(Ring*r){ pthread_mutex_lock(&r->mu); r->closed=1; pthread_cond_broadcast(&r->notempty); pthread_cond_broadcast(&r->notfull); pthread_mutex_unlock(&r->mu); }
/* CHAN: cap -> h */
static void op_chan(Ctx*cx){ int64_t cap=pop(cx).i; if(cap<=0)cap=16; Ring*r=(Ring*)uf_alloc(sizeof(Ring),0); r->tag=HT_RING; r->len=0; r->cap=(uint64_t)cap; r->buf=(Cell*)uf_alloc((size_t)cap*sizeof(Cell),0); r->head=0; r->tail=0; r->closed=0; pthread_mutex_init(&r->mu,0); pthread_cond_init(&r->notfull,0); pthread_cond_init(&r->notempty,0); pushp(cx,r); }
/* ENQ: h v ->  (blocks while full) */
static void op_enq(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Ring*r=(Ring*)((void*)h.i); if(r->tag!=HT_RING)die("ENQ: not a chan"); ring_enq(r,v); }
/* DEQ: h -> v  (blocks while empty; closed+empty yields sentinel 0) */
static void op_deq(Ctx*cx){ Cell h=pop(cx); Ring*r=(Ring*)((void*)h.i); if(r->tag!=HT_RING)die("DEQ: not a chan"); pthread_mutex_lock(&r->mu); while(r->len==0&&!r->closed) pthread_cond_wait(&r->notempty,&r->mu); Cell v; if(r->len==0){ v=uf_mki(0); } else { v=r->buf[r->head]; r->head=(r->head+1)%r->cap; r->len--; pthread_cond_signal(&r->notfull); } pthread_mutex_unlock(&r->mu); pushc(cx,v); }
static void op_close(Ctx*cx){ Cell h=pop(cx); Ring*r=(Ring*)((void*)h.i); if(r->tag!=HT_RING)die("CLOSE: not a chan"); ring_close(r); }

/* ATOM: atomic i64 cell */
static void op_atom(Ctx*cx){ Cell v=pop(cx); Atom*a=(Atom*)uf_alloc(sizeof(Atom),0); a->tag=HT_ATOM; a->len=1; atomic_store(&a->v,v.i); pushp(cx,a); }
static void op_aget(Ctx*cx){ Cell h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("AGET: not an atom"); pushi(cx,atomic_load(&a->v)); }
static void op_aset(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("ASET: not an atom"); atomic_store(&a->v,v.i); }
static void op_aadd(Ctx*cx){ Cell n=pop(cx),h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("AADD: not an atom"); pushi(cx,atomic_fetch_add(&a->v,n.i)); }
/* CAS: h old new -> 0/1 (new on top) */
static void op_cas(Ctx*cx){ Cell nw=pop(cx),old=pop(cx),h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("CAS: not an atom"); int64_t e=old.i; pushi(cx,atomic_compare_exchange_strong(&a->v,&e,nw.i)?1:0); }

/* generalized LEN + TYPEOF over tagged handles */
static void op_len(Ctx*cx){ Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); switch(a->tag){ case HT_ARR: case HT_TENSOR: case HT_DYN: case HT_MAP: case HT_RING: pushi(cx,(int64_t)a->len); return; case HT_ATOM: pushi(cx,1); return; default: die("LEN: handle has no length"); } }
static void op_typeof(Ctx*cx){ Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); pushi(cx,(int64_t)a->tag); }

/* reflection tables, populated by generated uf_init_reflection() */
static long uf_st_n=0; static const int64_t* uf_st_sids=0; static const int64_t* uf_st_nf=0; static const char*** uf_st_fields=0;

/* ---- wove-style task DAG scheduler ---- */
typedef void(*UfRun)(Ctx*,long);
typedef struct { long pc; int ninputs; int inputs[8]; Cell result; _Atomic int state; } WeaveTask;
typedef struct { WeaveTask* ts; int n; UfRun run; } WeaveJob;
static void* uf_worker(void*arg){
  WeaveJob*j=(WeaveJob*)arg;
  for(;;){
    int pick=-1;
    for(int i=0;i<j->n;i++){
      if(atomic_load(&j->ts[i].state)!=0) continue;
      int ready=1;
      for(int k=0;k<j->ts[i].ninputs;k++) if(atomic_load(&j->ts[j->ts[i].inputs[k]].state)!=2){ready=0;break;}
      if(!ready) continue;
      int exp=0;
      if(atomic_compare_exchange_strong(&j->ts[i].state,&exp,1)){ pick=i; break; }
    }
    if(pick<0){
      int alldone=1; for(int i=0;i<j->n;i++) if(atomic_load(&j->ts[i].state)!=2){alldone=0;break;}
      if(alldone) return 0;
      sched_yield(); continue;
    }
    WeaveTask*t=&j->ts[pick];
    Ctx*c=ctx_new(1<<16,1<<12);
    for(int k=0;k<t->ninputs;k++) pushc(c,j->ts[t->inputs[k]].result);
    j->run(c,t->pc);
    t->result = c->sp>0 ? c->ds[c->sp-1] : (Cell){T_INT,0,0,0};
    atomic_store(&t->state,2);
    free(c->ds); free((void*)c->cs); free(c);
  }
}
static void uf_weave(Ctx*cx,WeaveTask*ts,int n,UfRun run){
  (void)cx;
  long ncpu=sysconf(_SC_NPROCESSORS_ONLN);
  int nw=n; if(ncpu>0&&(long)nw>ncpu)nw=(int)ncpu; if(nw<1)nw=1; if(nw>64)nw=64;
  WeaveJob j={ts,n,run};
  if(nw<=1){ uf_worker(&j); return; }
  pthread_t th[64];
  for(int i=0;i<nw-1;i++) pthread_create(&th[i],0,uf_worker,&j);
  uf_worker(&j);
  for(int i=0;i<nw-1;i++) pthread_join(th[i],0);
}
/* FIELDS: obj -> dyn of interned field-name strings */
static void op_fields(Ctx*cx){
  Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); if(a->tag!=HT_OBJ)die("FIELDS: not an object");
  int64_t sid=(int64_t)a->len; const char**fs=0; int64_t nf=0;
  for(long q=0;q<uf_st_n;q++) if(uf_st_sids[q]==sid){ fs=uf_st_fields[q]; nf=uf_st_nf[q]; break; }
  if(!fs&&nf==0) die("FIELDS: unknown struct id");
  Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+(nf?nf:1)*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=nf?nf:1;
  for(int64_t q=0;q<nf;q++){ Cell c=uf_mkp((void*)fs[q]); d->data[d->len++]=c; }
  pushp(cx,d);
}

/* ================= shared string/list helpers ================= */
static char* uf_str_dup_n(const char*s,size_t n){ char*r=(char*)uf_alloc(n+1,0); memcpy(r,s,n); r[n]=0; return r; }
static Dyn* uf_dyn_new(uint64_t cap){ if(!cap)cap=1; Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+cap*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=cap; return d; }
static void uf_dyn_push(Dyn**pd,Cell c){ Dyn*d=*pd; if(d->len>=d->cap){ d->cap*=2; d=(Dyn*)realloc(d,sizeof(Dyn)+d->cap*sizeof(Cell)); if(!d)die("out of memory"); *pd=d; } d->data[d->len++]=c; }
static void uf_dyn_push_str(Dyn**pd,const char*s,size_t n){ char*p=uf_str_dup_n(s,n); Cell c=uf_mkp((void*)p); uf_dyn_push(pd,c); }
static char* uf_read_all(FILE*f){
  size_t cap=4096,n=0; char*b=(char*)uf_alloc(cap,0); size_t m;
  while((m=fread(b+n,1,cap-1-n,f))>0){ n+=m; if(cap-1-n==0){ cap*=2; b=(char*)realloc(b,cap); if(!b)die("out of memory"); } }
  b[n]=0; return b;
}
static int uf_wait_status(int r){
#ifdef _WIN32
  return r;
#else
  if(r==-1)return -1;
  if(WIFEXITED(r))return WEXITSTATUS(r);
  if(WIFSIGNALED(r))return 128+WTERMSIG(r);
  return r;
#endif
}

/* ================= shell ops ================= */
/* SH: cmd -> status (stdio inherited, platform shell) */
static void op_sh(Ctx*cx){ Cell c=pop(cx); pushi(cx,uf_wait_status(system((char*)((void*)c.i)))); }
/* SHX: cmd -> out err status (capture stdout+stderr; status on top) */
static void op_shx(Ctx*cx){
  Cell c=pop(cx); char*cmd=(char*)((void*)c.i);
#ifdef _WIN32
  char tmp[256]; tmpnam(tmp);
  char*full=(char*)uf_alloc(strlen(cmd)+strlen(tmp)+8,0);
  sprintf(full,"%s 2>%s",cmd,tmp);
  FILE* f=_popen(full,"r"); if(!f)die("SHX: spawn failed");
  char*out=uf_read_all(f); int st=uf_wait_status(_pclose(f));
  FILE* ef=fopen(tmp,"r"); char*err;
  if(ef){ err=uf_read_all(ef); fclose(ef); remove(tmp); } else err=uf_str_dup_n("",0);
  pushp(cx,out); pushp(cx,err); pushi(cx,st);
#else
  int pfd[2]; if(pipe(pfd))die("SHX: pipe");
  FILE* ef=tmpfile(); if(!ef)die("SHX: tmpfile");
  pid_t pid=fork();
  if(pid<0)die("SHX: fork");
  if(pid==0){
    close(pfd[0]);
    if(dup2(pfd[1],1)<0)_exit(127);
    if(dup2(fileno(ef),2)<0)_exit(127);
    execl("/bin/sh","sh","-c",cmd,(char*)0);
    _exit(127);
  }
  close(pfd[1]);
  FILE* f=fdopen(pfd[0],"r"); if(!f)die("SHX: fdopen");
  char*out=uf_read_all(f); fclose(f);
  int rs=0; waitpid(pid,&rs,0);
  int st=uf_wait_status(rs);
  rewind(ef); char*err=uf_read_all(ef); fclose(ef);
  pushp(cx,out); pushp(cx,err); pushi(cx,st);
#endif
}
/* SHL: cmd -> list (stdout split into lines; dies on spawn failure only) */
static void op_shl(Ctx*cx){
  Cell c=pop(cx);
#ifdef _WIN32
  FILE* f=_popen((char*)((void*)c.i),"r");
  if(!f)die("SHL: spawn failed");
  Dyn*d=uf_dyn_new(8); char line[16384];
  while(fgets(line,sizeof(line),f)){ size_t m=strlen(line); while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; uf_dyn_push_str(&d,line,m); }
  _pclose(f);
#else
  FILE* f=popen((char*)((void*)c.i),"r");
  if(!f)die("SHL: spawn failed");
  Dyn*d=uf_dyn_new(8);
  char*line=0; size_t ncap=0; ssize_t m;
  while((m=getline(&line,&ncap,f))>=0){ while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; uf_dyn_push_str(&d,line,(size_t)m); }
  free(line); pclose(f);
#endif
  pushp(cx,d);
}
/* SHP: cmd -> chan (worker thread streams stdout lines, closes chan at exit) */
typedef struct { Ring* r; char* cmd; } UfShp;
static void* uf_shp_worker(void*arg){
  UfShp* g=(UfShp*)arg;
#ifdef _WIN32
  FILE* f=_popen(g->cmd,"r");
#else
  FILE* f=popen(g->cmd,"r");
#endif
  if(f){
#ifdef _WIN32
    char line[16384];
    while(fgets(line,sizeof(line),f)){ size_t m=strlen(line); while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; char*p=uf_str_dup_n(line,m); Cell v=uf_mkp((void*)p); ring_enq(g->r,v); }
    _pclose(f);
#else
    char*line=0; size_t ncap=0; ssize_t m;
    while((m=getline(&line,&ncap,f))>=0){ while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; char*p=uf_str_dup_n(line,(size_t)m); Cell v=uf_mkp((void*)p); ring_enq(g->r,v); }
    free(line); pclose(f);
#endif
  }
  ring_close(g->r);
  free(g);
  return 0;
}
static void op_shp(Ctx*cx){
  Cell c=pop(cx);
  Ring*r=(Ring*)uf_alloc(sizeof(Ring),0); r->tag=HT_RING; r->len=0; r->cap=64; r->buf=(Cell*)uf_alloc(64*sizeof(Cell),0); r->head=0; r->tail=0; r->closed=0; pthread_mutex_init(&r->mu,0); pthread_cond_init(&r->notfull,0); pthread_cond_init(&r->notempty,0);
  UfShp* g=(UfShp*)malloc(sizeof(UfShp)); if(!g)die("out of memory"); g->r=r; g->cmd=(char*)((void*)c.i);
  pthread_t th; if(pthread_create(&th,0,uf_shp_worker,g)){ ring_close(r); die("SHP: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}
/* EXEC: list -> status (argv list, no shell; fork+execvp/waitpid or _spawnvp) */
static void op_exec(Ctx*cx){
  Cell h=pop(cx); Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("EXEC: not a list");
  if(d->len==0)die("EXEC: empty argv");
  char**argv=(char**)malloc((d->len+1)*sizeof(char*)); if(!argv)die("out of memory");
  for(uint64_t i=0;i<d->len;i++)argv[i]=(char*)((void*)d->data[i].i);
  argv[d->len]=0;
#ifdef _WIN32
  intptr_t r=_spawnvp(_P_WAIT,argv[0],(const char* const*)argv);
  int st=(r==-1)?-1:(int)r;
#else
  pid_t pid=fork();
  if(pid<0)die("EXEC: fork");
  if(pid==0){ execvp(argv[0],argv); _exit(127); }
  int rs=0; waitpid(pid,&rs,0); int st=uf_wait_status(rs);
#endif
  free(argv); pushi(cx,st);
}

/* ================= embedded regex =================
   Small backtracking engine (no deps). Syntax: literals, '.', '*', '+', '?',
   '[...]' (ranges, '^' negation, escape via '\'), '^' at alternative start,
   '$' at alternative end, '|' alternation, '(' ... ')' capture groups (<=9).
   Greedy matching with backtracking; a quantified atom never loops on an
   empty match. */
typedef struct { const char* s; const char* e; } RxCap;
enum { RXA_LIT=0, RXA_DOT=1, RXA_CLS=2, RXA_GRP=3 };
typedef struct { int type; char ch; const char* cs; const char* ce; const char* gs; const char* ge; int cap; } RxAtom;

/* p just after '[': find the closing ']' (']' first is literal, '\' escapes) */
static int rx_cls_find(const char* p, const char** close){
  if(*p=='^')p++;
  if(*p==']')p++;
  while(*p){
    if(*p=='\\'&&p[1]){ p+=2; continue; }
    if(*p==']'){ *close=p; return 1; }
    p++;
  }
  return 0;
}
/* class body [cs,ce): does it contain c? */
static int rx_cls_in(const char* cs, const char* ce, char c){
  int neg=0; const char* p=cs;
  if(p<ce&&*p=='^'){ neg=1; p++; }
  int ok=0; int first=1;
  while(p<ce){
    char lo;
    if(*p=='\\'&&p+1<ce){ lo=p[1]; p+=2; } else lo=*p++;
    if(first&&lo==']'){ if(c==']')ok=1; first=0; continue; }
    first=0;
    if(p<ce&&*p=='-'&&p+1<ce){
      p++; char hi;
      if(*p=='\\'&&p+1<ce){ hi=p[1]; p+=2; } else hi=*p++;
      if((unsigned char)c>=(unsigned char)lo&&(unsigned char)c<=(unsigned char)hi)ok=1;
    } else if(c==lo)ok=1;
  }
  return neg?!ok:ok;
}
/* number of '(' in [pat0,p), skipping classes and escapes */
static int rx_group_index(const char* pat0, const char* p){
  int n=0; const char* q=pat0;
  while(q<p){
    if(*q=='\\'&&q[1]){ q+=2; continue; }
    if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } }
    if(*q=='(')n++;
    q++;
  }
  return n;
}
static const char* rx_parse_atom(const char* p, RxAtom* a, const char* pat0){
  memset(a,0,sizeof(*a));
  char c=*p;
  if(c=='\\'){ if(!p[1])die("RX: trailing backslash"); a->type=RXA_LIT; a->ch=p[1]; return p+2; }
  if(c=='.'){ a->type=RXA_DOT; return p+1; }
  if(c=='['){ const char* cl; if(!rx_cls_find(p+1,&cl))die("RX: unbalanced ["); a->type=RXA_CLS; a->cs=p+1; a->ce=cl; return cl+1; }
  if(c=='('){
    int depth=1; const char* q=p+1;
    while(*q&&depth){
      if(*q=='\\'&&q[1]){ q+=2; continue; }
      if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } q++; continue; }
      if(*q=='(')depth++;
      else if(*q==')')depth--;
      q++;
    }
    if(depth)die("RX: unbalanced (");
    a->type=RXA_GRP; a->gs=p+1; a->ge=q-1; a->cap=rx_group_index(pat0,p)+1;
    if(a->cap>9)die("RX: more than 9 groups");
    return q;
  }
  a->type=RXA_LIT; a->ch=c; return p+1;
}
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0);
/* match one occurrence of atom a at s; NULL on no match */
static const char* rx_atom1(RxAtom* a, const char* s, RxCap* caps, const char* pat0){
  switch(a->type){
  case RXA_LIT: return (*s&&*s==a->ch)?s+1:0;
  case RXA_DOT: return *s?s+1:0;
  case RXA_CLS: return (*s&&rx_cls_in(a->cs,a->ce,*s))?s+1:0;
  case RXA_GRP: {
    const char* alt=a->gs;
    for(;;){
      const char* ae=alt; int depth=0;
      while(ae<a->ge){
        if(*ae=='\\'&&ae+1<a->ge){ ae+=2; continue; }
        if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)&&cl<a->ge){ ae=cl+1; continue; } }
        if(*ae=='(')depth++;
        else if(*ae==')')depth--;
        else if(*ae=='|'&&depth==0)break;
        ae++;
      }
      const char* r=rx_seq(alt,ae,s,caps,pat0);
      if(r){ caps[a->cap].s=s; caps[a->cap].e=r; return r; }
      if(ae>=a->ge)return 0;
      alt=ae+1;
    }
  }
  }
  return 0;
}
/* match pattern segment [p,pend) at s; end pointer on success, NULL on fail */
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0){
  if(p>=pend)return s;
  if(*p=='$'&&p+1==pend)return *s==0?s:0;
  RxAtom a; const char* q=rx_parse_atom(p,&a,pat0);
  long min=1,max=1;
  if(q<pend&&(*q=='*'||*q=='+'||*q=='?')){
    char t=*q; q++;
    if(t=='*'){min=0;max=1<<30;} else if(t=='+'){min=1;max=1<<30;} else {min=0;max=1;}
  }
  if(max==1){
    const char* r=rx_atom1(&a,s,caps,pat0);
    if(r){ const char* e=rx_seq(q,pend,r,caps,pat0); if(e)return e; }
    if(min==0)return rx_seq(q,pend,s,caps,pat0);
    return 0;
  }
  /* greedy repetition with backtracking; never loops on an empty match */
  size_t capn=16,n=0; const char**v=(const char**)malloc(capn*sizeof(char*));
  if(!v)die("out of memory");
  v[n++]=s;
  while((long)n-1<max){
    const char* r=rx_atom1(&a,v[n-1],caps,pat0);
    if(!r||r==v[n-1])break;
    if(n==capn){ capn*=2; v=(const char**)realloc(v,capn*sizeof(char*)); if(!v)die("out of memory"); }
    v[n++]=r;
  }
  const char* ok=0;
  for(long k=(long)n-1;k>=min;k--){
    const char* e=rx_seq(q,pend,v[k],caps,pat0);
    if(e){ ok=e; break; }
  }
  free(v);
  return ok;
}
/* try pattern (with top-level alternation) at/after str; fills caps (0=whole) */
static int rx_exec(const char* pat, const char* str, RxCap* caps){
  const char* alt=pat;
  for(;;){
    const char* ae=alt; int depth=0;
    while(*ae){
      if(*ae=='\\'&&ae[1]){ ae+=2; continue; }
      if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)){ ae=cl+1; continue; } }
      if(*ae=='(')depth++;
      else if(*ae==')')depth--;
      else if(*ae=='|'&&depth==0)break;
      ae++;
    }
    const char* p0=alt; int anch=0;
    if(p0<ae&&*p0=='^'){ anch=1; p0++; }
    const char* pos=str;
    for(;;){
      for(int i=0;i<10;i++){ caps[i].s=0; caps[i].e=0; }
      const char* r=rx_seq(p0,ae,pos,caps,pat);
      if(r){ caps[0].s=pos; caps[0].e=r; return 1; }
      if(anch||!*pos)break;
      pos++;
    }
    if(!*ae)return 0;
    alt=ae+1;
  }
}

/* RX: str pat -> list found (group 0 = whole match; found on top) */
static void op_rx(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=(char*)((void*)pat.i); const char* S=(char*)((void*)st.i);
  RxCap caps[10];
  int ntotal=rx_group_index(P,P+strlen(P));
  if(rx_exec(P,S,caps)){
    Dyn*d=uf_dyn_new((uint64_t)ntotal+1);
    for(int i=0;i<=ntotal;i++){
      if(caps[i].s) uf_dyn_push_str(&d,caps[i].s,(size_t)(caps[i].e-caps[i].s));
      else uf_dyn_push_str(&d,"",0);
    }
    pushp(cx,d); pushi(cx,1);
  } else {
    Dyn*d=uf_dyn_new(1); pushp(cx,d); pushi(cx,0);
  }
}
/* RXSUB: str pat repl -> str' (replace ALL matches; repl: \1..\9, \\) */
static void op_rxsub(Ctx*cx){
  Cell repl=pop(cx),pat=pop(cx),st=pop(cx);
  const char* R=(char*)((void*)repl.i); const char* P=(char*)((void*)pat.i); const char* S=(char*)((void*)st.i);
  size_t cap=256,n=0; char*out=(char*)uf_alloc(cap,0);
  RxCap caps[10];
  const char* cur=S;
#define UF_APP(src,L) do{ size_t _l=(size_t)(L); while(n+_l+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); } memcpy(out+n,(src),_l); n+=_l; }while(0)
  while(rx_exec(P,cur,caps)){
    UF_APP(cur,caps[0].s-cur);
    for(const char* r=R; *r; ){
      if(*r=='\\'&&r[1]){
        if(r[1]>='1'&&r[1]<='9'){ int g=r[1]-'0'; if(caps[g].s)UF_APP(caps[g].s,caps[g].e-caps[g].s); r+=2; }
        else { UF_APP(r+1,1); r+=2; }
      } else { UF_APP(r,1); r++; }
    }
    if(caps[0].e==caps[0].s){ if(!*cur)break; UF_APP(cur,1); cur++; }
    else cur=caps[0].e;
  }
  UF_APP(cur,strlen(cur));
#undef UF_APP
  out[n]=0; pushp(cx,out);
}
/* RXSPLIT: str pat -> list (pieces between matches; empty matches skipped) */
static void op_rxsplit(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=(char*)((void*)pat.i); const char* cur=(char*)((void*)st.i);
  Dyn*d=uf_dyn_new(8); RxCap caps[10];
  while(rx_exec(P,cur,caps)){
    if(caps[0].e==caps[0].s){ if(!*cur)break; cur++; continue; }
    uf_dyn_push_str(&d,cur,(size_t)(caps[0].s-cur));
    cur=caps[0].e;
  }
  uf_dyn_push_str(&d,cur,strlen(cur));
  pushp(cx,d);
}

/* ================= string ops ================= */
#ifdef _WIN32
/* fnmatch fallback: '*', '?', '[...]' (ranges, '!' negation) */
static int uf_glob_match(const char* pat,const char* s){
  while(*pat){
    if(*pat=='*'){
      while(*pat=='*')pat++;
      if(!*pat)return 1;
      for(const char* t=s;;t++){ if(uf_glob_match(pat,t))return 1; if(!*t)break; }
      return 0;
    }
    if(*pat=='?'){ if(!*s)return 0; pat++; s++; continue; }
    if(*pat=='['){
      const char* cl; if(rx_cls_find(pat+1,&cl)){
        if(!*s)return 0;
        int neg=0; const char* p=pat+1;
        if(p<cl&&*p=='!'){ neg=1; p++; }
        int ok=0;
        while(p<cl){
          char lo=*p++;
          if(p<cl&&*p=='-'&&p+1<cl){ p++; char hi=*p++; if((unsigned char)*s>=(unsigned char)lo&&(unsigned char)*s<=(unsigned char)hi)ok=1; }
          else if(*s==lo)ok=1;
        }
        if(neg)ok=!ok;
        if(!ok)return 0;
        pat=cl+1; s++; continue;
      }
    }
    if(*pat=='\\'&&pat[1])pat++;
    if(*pat!=*s)return 0;
    if(!*s)return 0;
    pat++; s++;
  }
  return *s==0;
}
#endif
/* GLOB: str pat -> 0/1 (fnmatch-style: '*', '?', '[...]') */
static void op_glob(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
#ifdef _WIN32
  pushi(cx,uf_glob_match((char*)((void*)pat.i),(char*)((void*)st.i))?1:0);
#else
  pushi(cx,fnmatch((char*)((void*)pat.i),(char*)((void*)st.i),0)==0?1:0);
#endif
}
/* SPLIT: str sep -> list (literal separator; pieces, tail included) */
static void op_split(Ctx*cx){
  Cell sep=pop(cx),st=pop(cx);
  const char* S=(char*)((void*)st.i); const char* E=(char*)((void*)sep.i);
  if(!*E)die("SPLIT: empty separator");
  size_t el=strlen(E);
  Dyn*d=uf_dyn_new(8);
  const char* cur=S;
  for(;;){
    const char* m=strstr(cur,E);
    if(!m)break;
    uf_dyn_push_str(&d,cur,(size_t)(m-cur));
    cur=m+el;
  }
  uf_dyn_push_str(&d,cur,strlen(cur));
  pushp(cx,d);
}
/* JOIN: list sep -> str (list of strings) */
static void op_join(Ctx*cx){
  Cell sep=pop(cx),h=pop(cx);
  Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("JOIN: not a list");
  const char* E=(char*)((void*)sep.i); size_t el=strlen(E);
  size_t cap=64; for(uint64_t i=0;i<d->len;i++)cap+=strlen((char*)((void*)d->data[i].i))+el;
  char*out=(char*)uf_alloc(cap,0); size_t n=0;
  for(uint64_t i=0;i<d->len;i++){
    if(i){ memcpy(out+n,E,el); n+=el; }
    size_t L=strlen((char*)((void*)d->data[i].i)); memcpy(out+n,(char*)((void*)d->data[i].i),L); n+=L;
  }
  out[n]=0; pushp(cx,out);
}
/* SLICE: str a b -> str' (Python slice: negatives from end, clamped; byte idx) */
static void op_slice(Ctx*cx){
  Cell b=pop(cx),a=pop(cx),st=pop(cx);
  const char* S=(char*)((void*)st.i); int64_t n=(int64_t)strlen(S);
  int64_t i=a.i,j=b.i;
  if(i<0)i+=n; if(j<0)j+=n;
  if(i<0)i=0; if(j<0)j=0;
  if(i>n)i=n; if(j>n)j=n;
  if(j<i)j=i;
  pushp(cx,uf_str_dup_n(S+i,(size_t)(j-i)));
}
/* FIND: str sub -> idx (-1 on miss; byte index) */
static void op_find(Ctx*cx){
  Cell sub=pop(cx),st=pop(cx);
  const char* m=strstr((char*)((void*)st.i),(char*)((void*)sub.i));
  pushi(cx,m?m-(char*)((void*)st.i):-1);
}
/* REPL: str old new -> str' (literal, replace all) */
static void op_repl(Ctx*cx){
  Cell nw=pop(cx),old=pop(cx),st=pop(cx);
  const char* S=(char*)((void*)st.i); const char* O=(char*)((void*)old.i); const char* N=(char*)((void*)nw.i);
  if(!*O)die("REPL: empty pattern");
  size_t ol=strlen(O),nl=strlen(N);
  size_t cap=strlen(S)+64,n=0; char*out=(char*)uf_alloc(cap,0);
  const char* cur=S;
  for(;;){
    const char* m=strstr(cur,O);
    if(!m)break;
    size_t pre=(size_t)(m-cur);
    while(n+pre+nl+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
    memcpy(out+n,cur,pre); n+=pre; memcpy(out+n,N,nl); n+=nl;
    cur=m+ol;
  }
  size_t tail=strlen(cur);
  while(n+tail+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
  memcpy(out+n,cur,tail); n+=tail;
  out[n]=0; pushp(cx,out);
}
/* TRIM: str -> str' (isspace, both ends) */
static void op_trim(Ctx*cx){
  Cell st=pop(cx);
  const char* s=(char*)((void*)st.i); size_t n=strlen(s);
  while(n>0&&isspace((unsigned char)s[0])){ s++; n--; }
  while(n>0&&isspace((unsigned char)s[n-1]))n--;
  pushp(cx,uf_str_dup_n(s,n));
}
/* UP/DOWN: str -> str' (ASCII case) */
static void op_up(Ctx*cx){ Cell st=pop(cx); char*s=(char*)((void*)st.i); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='a'&&s[i]<='z')?(char)(s[i]-32):s[i]; r[n]=0; pushp(cx,r); }
static void op_down(Ctx*cx){ Cell st=pop(cx); char*s=(char*)((void*)st.i); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='A'&&s[i]<='Z')?(char)(s[i]+32):s[i]; r[n]=0; pushp(cx,r); }
/* STARTS/ENDS: str affix -> 0/1 */
static void op_starts(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=(char*)((void*)st.i); const char*a=(char*)((void*)af.i); pushi(cx,strncmp(s,a,strlen(a))==0?1:0); }
static void op_ends(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=(char*)((void*)st.i); const char*a=(char*)((void*)af.i); size_t ls=strlen(s),la=strlen(a); pushi(cx,(la<=ls&&strcmp(s+ls-la,a)==0)?1:0); }
"#;
