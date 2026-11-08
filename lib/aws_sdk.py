#!/usr/bin/env python3
"""Restricted AWS SDK transport for the benchmark orchestrator (no AWS CLI)."""
import base64
import json
import os
import re
import subprocess
import sys


def parse_options(args):
    options = {}
    key = None
    for arg in args:
        if arg.startswith('--'):
            key = arg[2:]
            if key in options:
                raise ValueError(f'repeated option: {key}')
            options[key] = []
        elif key is None:
            raise ValueError('expected an option')
        else:
            options[key].append(arg)
    return options


def request(operation, options):
    """Translate only the small vocabulary used by run-on-ec2.sh."""
    mapping = {'instance-id': 'InstanceId', 'instance-ids': 'InstanceIds', 'image-id': 'ImageId', 'instance-type': 'InstanceType',
               'key-name': 'KeyName', 'key-names': 'KeyNames', 'group-name': 'GroupName',
               'description': 'Description', 'group-id': 'GroupId', 'security-group-ids': 'SecurityGroupIds',
               'name': 'Name', 'instance-initiated-shutdown-behavior': 'InstanceInitiatedShutdownBehavior',
               'vpc-id': 'VpcId', 'subnet-id': 'SubnetId', 'user-data': 'UserData'}
    arrays = {'instance-ids', 'key-names', 'security-group-ids'}
    result = {}
    for key, values in options.items():
        if key in ('query', 'output'):
            continue
        if key in mapping:
            if not values or (key not in arrays and len(values) != 1):
                raise ValueError(f'invalid {key}')
            result[mapping[key]] = values if key in arrays else values[0]
        elif key == 'filters':
            result['Filters'] = []
            for value in values:
                match = re.fullmatch(r'Name=([^,]+),Values=(.+)', value)
                if not match:
                    raise ValueError('invalid filter')
                result['Filters'].append({'Name': match[1], 'Values': match[2].split(',')})
        elif key in ('tag-specifications', 'block-device-mappings'):
            if len(values) != 1:
                raise ValueError(f'{key} requires one JSON array')
            result[{'tag-specifications': 'TagSpecifications', 'block-device-mappings': 'BlockDeviceMappings'}[key]] = json.loads(values[0])
        elif key == 'latest':
            if values: raise ValueError('latest takes no value')
            result['Latest'] = True
        elif key in ('protocol', 'port', 'cidr'):
            continue
        else:
            raise ValueError(f'unsupported option: {key}')
    if operation == 'authorize-security-group-ingress':
        result['IpPermissions'] = [{'IpProtocol': options['protocol'][0],
                                   'FromPort': int(options['port'][0]), 'ToPort': int(options['port'][0]),
                                   'IpRanges': [{'CidrIp': options['cidr'][0]}]}]
    if operation == 'run-instances':
        result.update(MinCount=1, MaxCount=1, MetadataOptions={'HttpTokens': 'required', 'HttpEndpoint': 'enabled'})
    if operation == 'create-security-group':
        tags = [{'Key': 'purpose', 'Value': 'gateway-bench'}, {'Key': 'managed-by', 'Value': 'gateway-bench'}]
        if os.environ.get('OTB_BENCH_RUN_ID'):
            tags.append({'Key': 'run', 'Value': os.environ['OTB_BENCH_RUN_ID']})
        result['TagSpecifications'] = [{'ResourceType': 'security-group', 'Tags': tags}]
    return result


def expected_account():
    account = os.environ.get('OTB_AWS_ACCOUNT_ID', '')
    if not re.fullmatch(r'\d{12}', account):
        raise ValueError('Set OTB_AWS_ACCOUNT_ID to the explicitly selected AWS account')
    return account


def session(region=None):
    """Explicit account + standard SDK credentials, or an optional local Keychain service."""
    import boto3
    account = expected_account()
    region = region or os.environ.get('OTB_AWS_REGION') or os.environ.get('AWS_REGION')
    service = os.environ.get('OTB_AWS_KEYCHAIN_SERVICE')
    profile = os.environ.get('OTB_AWS_PROFILE')
    if service and profile:
        raise ValueError('Choose a profile or Keychain service, not both')
    if service:
        secret = subprocess.check_output(['security', 'find-generic-password', '-s', service, '-w'], text=True)
        credentials = json.loads(secret)
        if credentials.get('account_id') != account:
            raise ValueError('Keychain credential account differs from the expected account')
        return boto3.Session(aws_access_key_id=credentials['aws_access_key_id'],
                             aws_secret_access_key=credentials['aws_secret_access_key'],
                             aws_session_token=credentials.get('aws_session_token'),
                             region_name=region or credentials.get('region_name'))
    if not profile and not (os.environ.get('AWS_ACCESS_KEY_ID') or os.environ.get('AWS_WEB_IDENTITY_TOKEN_FILE')
                            or os.environ.get('AWS_CONTAINER_CREDENTIALS_RELATIVE_URI')
                            or os.environ.get('AWS_CONTAINER_CREDENTIALS_FULL_URI')
                            or os.environ.get('OTB_AWS_USE_INSTANCE_ROLE') == '1'):
        raise ValueError('Select OTB_AWS_PROFILE or provide environment/role credentials explicitly')
    return boto3.Session(profile_name=profile, region_name=region)


def verify_account(sdk):
    if sdk.client('sts').get_caller_identity()['Account'] != expected_account():
        raise ValueError('AWS account mismatch')


def main(args):
    import jmespath
    service, operation, *args = args
    allowed = {'ec2': {'describe-instances', 'describe-key-pairs', 'create-key-pair', 'delete-key-pair',
                       'create-security-group', 'delete-security-group', 'authorize-security-group-ingress',
                       'run-instances', 'terminate-instances', 'get-console-output', 'wait'}, 'ssm': {'get-parameter'}}
    if operation not in allowed.get(service, set()):
        raise ValueError('unsupported service or operation')
    waiter = args.pop(0) if operation == 'wait' else None
    options = parse_options(args)
    sdk = session()
    if not operation.startswith('describe-') and operation not in ('get-parameter','get-console-output','wait'):
        verify_account(sdk)
    client = sdk.client(service)
    parameters = request(operation, options)
    if waiter:
        if waiter not in ('instance-running', 'instance-terminated'):
            raise ValueError('unsupported waiter')
        client.get_waiter(waiter.replace('-', '_')).wait(**parameters)
        return
    response = getattr(client, operation.replace('-', '_'))(**parameters)
    # Botocore decodes EC2 console Output through its response handler.
    if 'query' in options:
        response = jmespath.search(options['query'][0], response)
    if options.get('output') == ['text']:
        def text(value):
            if isinstance(value, list): return '\n'.join('\t'.join(map(str, row)) if isinstance(row, list) else str(row) for row in value)
            return 'None' if value is None else str(value)
        print(text(response))
    else:
        print(json.dumps(response, default=str))


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except Exception as exc:
        print('benchmark AWS request failed: ' + str(exc).split('Encoded authorization failure message:')[0], file=sys.stderr)
        sys.exit(1)
