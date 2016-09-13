import React, { Component, PropTypes } from 'react';
import { connect } from 'react-redux';
import { bindActionCreators } from 'redux';
import { MenuItem, Toggle } from 'material-ui';

import IdentityIcon from '../../IdentityIcon';
import InputAddress from '../InputAddress';
import Select from '../Select';

import styles from './inputAddressSelect.css';

class InputAddressSelect extends Component {
  static propTypes = {
    entries: PropTypes.array,
    disabled: PropTypes.bool,
    error: PropTypes.string,
    label: PropTypes.string,
    hint: PropTypes.string,
    value: PropTypes.string,
    onChange: PropTypes.func
  };

  state = {
    editing: false,
    entries: []
  }

  render () {
    const { editing } = this.state;

    return (
      <div className={ styles.inputselect }>
        { editing ? this.renderInput() : this.renderSelect() }
        <Toggle
          className={ styles.toggle }
          label='Edit'
          labelPosition='right'
          toggled={ editing }
          onToggle={ this.onToggle } />
      </div>
    );
  }

  renderInput () {
    const { disabled, error, hint, label, value } = this.props;

    return (
      <InputAddress
        disabled={ disabled }
        error={ error }
        hint={ hint }
        label={ label }
        value={ value }
        onChange={ this.onChangeInput } />
    );
  }

  renderSelect () {
    const { disabled, error, hint, label, value } = this.props;

    return (
      <Select
        disabled={ disabled }
        label={ label }
        hint={ hint }
        error={ error }
        value={ value }
        onChange={ this.onChangeSelect }>
        { this.renderSelectAccounts() }
      </Select>
    );
  }

  renderAccountItem = (account) => {
    const item = (
      <div className={ styles.account }>
        <div className={ styles.image }>
          <IdentityIcon
            inline center
            address={ account.address } />
        </div>
        <div className={ styles.details }>
          <div className={ styles.name }>
            { account.name || 'Unnamed' }
          </div>
        </div>
      </div>
    );

    return (
      <MenuItem
        key={ account.address }
        value={ account.address }
        label={ item }>
        { item }
      </MenuItem>
    );
  }

  renderSelectAccounts () {
    const { entries } = this.props;

    return entries.map(this.renderAccountItem);
  }

  onToggle = () => {
    const { editing } = this.state;

    this.setState({
      editing: !editing
    });
  }

  onChangeInput = (event, value) => {
    this.props.onChange(event, value);
  }

  onChangeSelect = (event, idx) => {
    const { entries } = this.props;
    console.log(entries[idx].address);

    this.props.onChange(event, entries[idx].address);
  }
}

function mapStateToProps (state) {
  const { accounts, contacts } = state.personal;

  return {
    entries: Object.values(accounts).concat(Object.values(contacts))
  };
}

function mapDispatchToProps (dispatch) {
  return bindActionCreators({}, dispatch);
}

export default connect(
  mapStateToProps,
  mapDispatchToProps
)(InputAddressSelect);