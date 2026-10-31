// Shared CIE 1960 white-point model for the linear RAW API. The isotherm data
// are from Wyszecki & Stiles, Color Science, 2nd ed., p. 228 (Robertson table).
// Positive Tint moves towards magenta, using the public DNG distance scale.
#pragma once
#include <algorithm>
#include <cmath>

namespace fc_wb {
struct Isotherm { double mired,u,v,slope; };
static const Isotherm isotherms[] = {
#include "white_balance_table.inc"
};
inline void direction(const Isotherm &line,double &du,double &dv) {
    const double length=std::sqrt(1+line.slope*line.slope);
    du=1/length;dv=line.slope/length;
}
inline bool white_uv(double kelvin,double tint,double &u,double &v) {
    if(!std::isfinite(kelvin)||!std::isfinite(tint)||kelvin<2000||kelvin>50000||std::abs(tint)>150)return false;
    const double reciprocal=1e6/kelvin;
    for(unsigned i=0;i<30;++i)if(reciprocal<isotherms[i+1].mired||i==29){
        const auto &a=isotherms[i],&b=isotherms[i+1];
        const double w=(b.mired-reciprocal)/(b.mired-a.mired);
        double au,av,bu,bv;direction(a,au,av);direction(b,bu,bv);
        double du=w*au+(1-w)*bu,dv=w*av+(1-w)*bv;
        const double length=std::sqrt(du*du+dv*dv);
        u=w*a.u+(1-w)*b.u-tint*du/(3000*length);
        v=w*a.v+(1-w)*b.v-tint*dv/(3000*length);
        return std::isfinite(u)&&std::isfinite(v);
    }
    return false;
}
inline bool from_uv(double u,double v,double &kelvin,double &tint) {
    if(!std::isfinite(u)||!std::isfinite(v))return false;
    double previous_distance=0,previous_du=0,previous_dv=0;
    for(unsigned i=1;i<31;++i){
        const auto &line=isotherms[i];double du,dv;direction(line,du,dv);
        double distance=-(u-line.u)*dv+(v-line.v)*du;
        if(distance<=0||i==30){
            distance=std::max(0.,-distance);
            const double w=i==1?0.:distance/(previous_distance+distance);
            const auto &previous=isotherms[i-1];
            const double reciprocal=w*previous.mired+(1-w)*line.mired;
            if(reciprocal<=0)return false;
            kelvin=1e6/reciprocal;
            const double delta_u=u-(w*previous.u+(1-w)*line.u);
            const double delta_v=v-(w*previous.v+(1-w)*line.v);
            du=w*previous_du+(1-w)*du;dv=w*previous_dv+(1-w)*dv;
            const double length=std::sqrt(du*du+dv*dv);
            if(length<=0)return false;
            tint=-3000*(delta_u*du+delta_v*dv)/length;
            return std::isfinite(kelvin)&&std::isfinite(tint)&&kelvin>=1999&&kelvin<=50001&&std::abs(tint)<=150.1;
        }
        previous_distance=distance;previous_du=du;previous_dv=dv;
    }
    return false;
}
inline bool multipliers(const float matrix[4][3],double kelvin,double tint,float result[4]) {
    double u,v;if(!white_uv(kelvin,tint,u,v))return false;
    const double denominator=2*u-8*v+4;
    const double x=3*u/denominator,y=2*v/denominator;
    if(!std::isfinite(x)||!std::isfinite(y)||x<=0||y<=0||x+y>=1)return false;
    const double xyz[]={x/y,1.,(1-x-y)/y};
    for(int c=0;c<3;++c){
        double response=0;for(int k=0;k<3;++k)response+=matrix[c][k]*xyz[k];
        if(!std::isfinite(response)||response<=0.00001)return false;
        result[c]=float(1/response);
    }
    const float green=result[1];for(int c=0;c<3;++c)result[c]/=green;
    result[3]=1;return true;
}
inline bool estimate(const float matrix[4][3],const float multipliers[4],double &kelvin,double &tint) {
    double equations[3][4]={};
    for(int c=0;c<3;++c){
        if(!std::isfinite(multipliers[c])||multipliers[c]<=0)return false;
        for(int k=0;k<3;++k)equations[c][k]=matrix[c][k];
        equations[c][3]=1/multipliers[c];
    }
    for(int column=0;column<3;++column){
        int pivot=column;for(int row=column+1;row<3;++row)if(std::abs(equations[row][column])>std::abs(equations[pivot][column]))pivot=row;
        if(std::abs(equations[pivot][column])<1e-10)return false;
        for(int k=0;k<4;++k)std::swap(equations[pivot][k],equations[column][k]);
        const double divisor=equations[column][column];for(int k=0;k<4;++k)equations[column][k]/=divisor;
        for(int row=0;row<3;++row)if(row!=column){const double n=equations[row][column];for(int k=0;k<4;++k)equations[row][k]-=n*equations[column][k];}
    }
    const double X=equations[0][3],Y=equations[1][3],Z=equations[2][3],d=X+15*Y+3*Z;
    return X>0&&Y>0&&Z>0&&d>0&&from_uv(4*X/d,6*Y/d,kelvin,tint);
}
} // namespace fc_wb
