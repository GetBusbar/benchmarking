#!/usr/bin/env python3
"""Choose from current AWS on-demand prices without changing the benchmark hardware."""
import datetime
from decimal import Decimal


def products(pricing, service, filters):
    import json
    for page in pricing.get_paginator('get_products').paginate(ServiceCode=service, Filters=[{'Type':'TERM_MATCH','Field':k,'Value':v} for k,v in filters.items()]):
        for raw in page['PriceList']:
            yield json.loads(raw)


def price(product, unit):
    prices=[]
    for term in product['terms'].get('OnDemand',{}).values():
        for dimension in term['priceDimensions'].values():
            if dimension['unit']==unit and 'USD' in dimension['pricePerUnit']:
                prices.append(Decimal(dimension['pricePerUnit']['USD']))
    return min(prices) if prices else None


def live_prices(sdk, instance_type='m7g.4xlarge', disk_gb=60):
    pricing=sdk.client('pricing',region_name='us-east-1')  # API endpoint, not placement.
    enabled={r['RegionName'] for r in sdk.client('ec2').describe_regions()['Regions']}
    storage={}
    for product in products(pricing,'AmazonEC2',{'volumeApiName':'gp3','productFamily':'Storage'}):
        region=product['product']['attributes'].get('regionCode'); amount=price(product,'GB-Mo')
        if region in enabled and amount is not None: storage[region]=amount
    ipv4={}
    for product in products(pricing,'AmazonVPC',{'group':'VPCPublicIPv4Address'}):
        attributes=product['product']['attributes']; region=attributes.get('regionCode')
        if 'InUse' not in attributes.get('usagetype',''):continue
        amount=price(product,'Hrs')
        if region in enabled and amount is not None:ipv4[region]=amount
    rows=[]
    for product in products(pricing,'AmazonEC2',{'instanceType':instance_type,'operatingSystem':'Linux','tenancy':'Shared','preInstalledSw':'NA','capacitystatus':'Used'}):
        attrs=product['product']['attributes']; region=attrs.get('regionCode'); amount=price(product,'Hrs')
        if region not in enabled or amount is None:continue
        if region not in storage or region not in ipv4:
            continue  # Unknown costs are never silently treated as free.
        total=amount+storage[region]*Decimal(disk_gb)/Decimal(730)+ipv4[region]
        rows.append({'region':region,'instance_type':instance_type,'instance_hour_usd':float(amount),'gp3_gb_month_usd':float(storage[region]),'ipv4_hour_usd':float(ipv4[region]),'estimated_hour_usd':float(total),'disk_gb':disk_gb,'checked_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'source':'AWS Price List Query API','excludes':'taxes and variable data transfer; gp3 monthly rate converted using 730 hours'})
    if not rows:raise ValueError('No complete live compute/storage/IPv4 price found for enabled regions')
    return sorted(rows,key=lambda r:(r['estimated_hour_usd'],r['region']))


def capacity(sdk, region, instance_type):
    ec2=sdk.client('ec2',region_name=region)
    zones=ec2.describe_instance_type_offerings(LocationType='availability-zone',Filters=[{'Name':'instance-type','Values':[instance_type]}])['InstanceTypeOfferings']
    if not zones:return {'slots':0,'reason':'instance type unavailable'}
    quota=sdk.client('service-quotas',region_name=region).get_service_quota(ServiceCode='ec2',QuotaCode='L-1216C47A')['Quota']['Value']
    active=ec2.describe_instances(Filters=[{'Name':'instance-state-name','Values':['pending','running']}])
    types=[i['InstanceType'] for r in active['Reservations'] for i in r['Instances'] if i.get('InstanceLifecycle')!='spot']
    cores={}
    if types:
        cores={t['InstanceType']:t['VCpuInfo']['DefaultVCpus'] for t in ec2.describe_instance_types(InstanceTypes=list(set(types)))['InstanceTypes']}
    vcpus=ec2.describe_instance_types(InstanceTypes=[instance_type])['InstanceTypes'][0]['VCpuInfo']['DefaultVCpus']
    return {'slots':max(0,int((quota-sum(cores[t] for t in types))//vcpus)),'zone':sorted(z['Location'] for z in zones)[0],'quota_vcpus':quota}


def select(gateways, prices, capacities, mode):
    remaining={region:data['slots'] for region,data in capacities.items()}
    if mode=='same-region':
        eligible=next((p for p in prices if remaining.get(p['region'],0)>=len(gateways)),None)
        if eligible is None:raise ValueError('No priced region has quota for the complete field')
        prices=[eligible]
    assignments=[]
    for gateway in gateways:
        best=next((p for p in prices if remaining.get(p['region'],0)>0),None)
        if best is None:raise ValueError('Insufficient eligible on-demand quota for the field')
        remaining[best['region']]-=1
        assignments.append({**best,'gateway':gateway,'zone':capacities[best['region']]['zone'],'placement_mode':mode})
    return assignments
