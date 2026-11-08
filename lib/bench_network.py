#!/usr/bin/env python3
"""Journaled ephemeral EC2 network. Never adopts or deletes another workload's VPC."""
import json
from pathlib import Path

PURPOSE = 'gateway-bench'


def tags(run_id):
    return [{'Key': 'purpose', 'Value': PURPOSE}, {'Key': 'run', 'Value': run_id},
            {'Key': 'Name', 'Value': f'gateway-bench-{run_id}'}]


def save(path, state):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix('.writing')
    temporary.write_text(json.dumps(state, indent=2) + '\n')
    temporary.replace(path)


def create(client, run_id, zone, journal):
    state = {'run_id': run_id, 'zone': zone, 'region': client.meta.region_name}
    def tagged(kind):
        return [{'ResourceType': kind, 'Tags': tags(run_id)}]
    def record(key, value):
        state[key] = value
        save(journal, state)
        return value
    save(journal, state)
    try:
        vpc = record('vpc_id', client.create_vpc(CidrBlock='10.240.0.0/24', TagSpecifications=tagged('vpc'))['Vpc']['VpcId'])
        client.get_waiter('vpc_available').wait(VpcIds=[vpc])
        client.modify_vpc_attribute(VpcId=vpc, EnableDnsSupport={'Value': True})
        client.modify_vpc_attribute(VpcId=vpc, EnableDnsHostnames={'Value': True})
        subnet = record('subnet_id', client.create_subnet(VpcId=vpc, CidrBlock='10.240.0.0/24', AvailabilityZone=zone, TagSpecifications=tagged('subnet'))['Subnet']['SubnetId'])
        client.modify_subnet_attribute(SubnetId=subnet, MapPublicIpOnLaunch={'Value': True})
        igw = record('internet_gateway_id', client.create_internet_gateway(TagSpecifications=tagged('internet-gateway'))['InternetGateway']['InternetGatewayId'])
        client.attach_internet_gateway(InternetGatewayId=igw, VpcId=vpc)
        table = record('route_table_id', client.create_route_table(VpcId=vpc, TagSpecifications=tagged('route-table'))['RouteTable']['RouteTableId'])
        client.create_route(RouteTableId=table, DestinationCidrBlock='0.0.0.0/0', GatewayId=igw)
        record('association_id', client.associate_route_table(RouteTableId=table, SubnetId=subnet)['AssociationId'])
        record('ready', True)
        return state
    except Exception:
        cleanup(client, state, journal)
        raise


def cleanup(client, state, journal):
    """Instances/owned security groups must be gone before deleting their network."""
    vpc = state.get('vpc_id')
    if not vpc:
        return
    found = client.describe_vpcs(Filters=[{'Name': 'vpc-id', 'Values': [vpc]}])['Vpcs']
    if not found:
        state['deleted'] = True
        save(journal, state)
        return
    owned = {v['Key']: v['Value'] for v in found[0].get('Tags', [])}
    if owned.get('purpose') != PURPOSE or owned.get('run') != state['run_id']:
        raise ValueError('Refusing to delete a VPC not owned by this benchmark run')
    instances = client.describe_instances(Filters=[{'Name': 'vpc-id', 'Values': [vpc]}, {'Name': 'instance-state-name', 'Values': ['pending', 'running', 'stopping', 'stopped', 'shutting-down']}])
    if any(r['Instances'] for r in instances['Reservations']):
        raise RuntimeError('Network still has instances; preserve it until harvest and termination complete')
    for key, operation, argument in [('association_id', 'disassociate_route_table', 'AssociationId'),
                                     ('route_table_id', 'delete_route_table', 'RouteTableId'),
                                     ('subnet_id', 'delete_subnet', 'SubnetId')]:
        if state.get(key):
            getattr(client, operation)(**{argument: state[key]})
            state.pop(key)
            save(journal, state)
    if state.get('internet_gateway_id'):
        igw = state['internet_gateway_id']
        attached = client.describe_internet_gateways(InternetGatewayIds=[igw])['InternetGateways'][0]['Attachments']
        if attached:
            client.detach_internet_gateway(InternetGatewayId=igw, VpcId=vpc)
        client.delete_internet_gateway(InternetGatewayId=igw)
        state.pop('internet_gateway_id')
        save(journal, state)
    client.delete_vpc(VpcId=vpc)
    state['deleted'] = True
    save(journal, state)
